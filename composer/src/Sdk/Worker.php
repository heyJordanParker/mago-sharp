<?php

declare(strict_types=1);

namespace Mago\Sdk;

use Closure;
use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AssertionProviderContext;
use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\CallableSignatureProvider;
use Mago\Sdk\Analyzer\CallableSignatureProviderContext;
use Mago\Sdk\Analyzer\CallForwardingProvider;
use Mago\Sdk\Analyzer\CallForwardingProviderContext;
use Mago\Sdk\Analyzer\ClassInitializerProvider;
use Mago\Sdk\Analyzer\ClassInitializerProviderContext;
use Mago\Sdk\Analyzer\ClassLikeTarget;
use Mago\Sdk\Analyzer\ClassTarget;
use Mago\Sdk\Analyzer\Codebase;
use Mago\Sdk\Analyzer\CodebaseScanContext;
use Mago\Sdk\Analyzer\CodebaseScanHook;
use Mago\Sdk\Analyzer\FileAnalysis;
use Mago\Sdk\Analyzer\FunctionAssertionProvider;
use Mago\Sdk\Analyzer\FunctionReturnTypeProvider;
use Mago\Sdk\Analyzer\FunctionTarget;
use Mago\Sdk\Analyzer\InitializationContext;
use Mago\Sdk\Analyzer\InvocationKind;
use Mago\Sdk\Analyzer\IssueFilterContext;
use Mago\Sdk\Analyzer\IssueFilterDecision;
use Mago\Sdk\Analyzer\IssueFilterHook;
use Mago\Sdk\Analyzer\MethodAssertionProvider;
use Mago\Sdk\Analyzer\MethodReturnTypeProvider;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\NodeAnalysisContext;
use Mago\Sdk\Analyzer\NodeAnalysisHook;
use Mago\Sdk\Analyzer\PluginRegistry as AnalyzerPluginRegistry;
use Mago\Sdk\Analyzer\ProjectAnalysis;
use Mago\Sdk\Analyzer\PropertyInitializationProvider;
use Mago\Sdk\Analyzer\PropertyInitializationProviderContext;
use Mago\Sdk\Analyzer\PropertyTarget;
use Mago\Sdk\Analyzer\PropertyTypeProvider;
use Mago\Sdk\Analyzer\PropertyTypeProviderContext;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\TargetedAnalysisHook;
use Mago\Sdk\Analyzer\TargetedProvider;
use Mago\Sdk\Analyzer\TypeComparator;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Analyzer\CodebaseReads;
use Mago\Sdk\Internal\Analyzer\DefinitionName;
use Mago\Sdk\Internal\Analyzer\MetadataCache;
use Mago\Sdk\Internal\Analyzer\Protocol as AnalyzerProtocol;
use Mago\Sdk\Internal\Analyzer\RegisteredPlugin;
use Mago\Sdk\Internal\Analyzer\RegisteredTargetedCallback;
use Mago\Sdk\Internal\Analyzer\ReportedIssue;
use Mago\Sdk\Internal\Analyzer\SentReads;
use Mago\Sdk\Internal\ClassLoads;
use Mago\Sdk\Internal\HostClient;
use Mago\Sdk\Internal\Hosts;
use Mago\Sdk\Internal\Linter\Protocol as LinterProtocol;
use Mago\Sdk\Internal\Linter\RegisteredRule;
use Mago\Sdk\Internal\Protocol\ValueReader;
use Mago\Sdk\Internal\RuleTimes;
use Mago\Sdk\Internal\Worker\Protocol as WorkerProtocol;
use Mago\Sdk\Linter\LintContext;
use Mago\Sdk\Syntax\NodeKind;
use Throwable;

use function array_key_exists;
use function array_map;
use function array_values;
use function count;
use function function_exists;
use function implode;
use function in_array;
use function is_array;
use function str_contains;
use function strtolower;

/**
 * Serves one or more Mago extensions from the file of an extension host, which mago runs in the interpreter of each analysis thread.
 *
 * @api
 */
final class Worker
{
    /**
     * @var non-empty-list<Extension>
     */
    private readonly array $extensions;

    /**
     * @var list<RegisteredRule>
     */
    private readonly array $rules;

    /** @var list<RegisteredPlugin> */
    private readonly array $analyzerPlugins;

    /** @var list<int<0, max>> */
    private readonly array $workerReducerIndices;

    /** @var list<RegisteredTargetedCallback<FunctionReturnTypeProvider, FunctionTarget>> */
    private readonly array $functionReturnTypeProviders;

    /** @var list<RegisteredTargetedCallback<MethodReturnTypeProvider, MethodTarget>> */
    private readonly array $methodReturnTypeProviders;

    /** @var list<RegisteredTargetedCallback<FunctionAssertionProvider, FunctionTarget>> */
    private readonly array $functionAssertionProviders;

    /** @var list<RegisteredTargetedCallback<MethodAssertionProvider, MethodTarget>> */
    private readonly array $methodAssertionProviders;

    /** @var list<RegisteredTargetedCallback<PropertyTypeProvider, PropertyTarget>> */
    private readonly array $propertyTypeProviders;

    /** @var list<RegisteredTargetedCallback<PropertyInitializationProvider, PropertyTarget>> */
    private readonly array $propertyInitializationProviders;

    /** @var list<RegisteredTargetedCallback<ClassInitializerProvider, ClassTarget>> */
    private readonly array $classInitializerProviders;

    /** @var list<RegisteredTargetedCallback<CallForwardingProvider, MethodTarget>> */
    private readonly array $callForwardingProviders;

    /** @var list<RegisteredTargetedCallback<IssueFilterHook, string>> */
    private readonly array $issueFilterHooks;

    /** @var list<RegisteredTargetedCallback<CodebaseScanHook, string>> */
    private readonly array $codebaseScanHooks;

    /**
     * The identifiers of the plugins that register a provider. A provider may answer from anything its plugin read, in
     * any callback, so what such a plugin reads can change the analyzed code.
     *
     * @var array<string, true>
     */
    private readonly array $pluginsWithProviders;

    /**
     * @var list<int<0, 65535>>
     */
    private array $activeRuleIndices = [];

    /**
     * @var array<string, list<RegisteredRule>>
     */
    private array $activeRulesByNodeKind = [];

    /**
     * @var list<NodeKind>
     */
    private array $nodeKinds = [];

    private PHPVersion $phpVersion;

    private ?MetadataCache $metadataCache = null;

    private readonly ClassLoads $classLoads;

    private readonly SentReads $sentReads;

    public function __construct(Extension $extension, Extension ...$additionalExtensions)
    {
        $this->classLoads = Hosts::classLoads();
        $this->sentReads = Hosts::sentReads();
        $extensions = [$extension, ...array_values($additionalExtensions)];
        $extensionIdentifiers = [];
        $ruleCodes = [];
        $rules = [];
        $pluginSelectors = [];
        $registeredPlugins = [];
        $functionProviders = [];
        $methodProviders = [];
        $functionAssertionProviders = [];
        $methodAssertionProviders = [];
        $propertyProviders = [];
        $propertyInitializationProviders = [];
        $classInitializerProviders = [];
        $callForwardingProviders = [];
        $issueFilterHooks = [];
        $codebaseScanHooks = [];
        $nodeAnalysisHooks = [];
        $methodCallAnalysisHooks = [];
        $classLikeAnalysisHooks = [];
        $pluginsWithProviders = [];
        $workerReducerIndices = [];
        foreach ($extensions as $extensionIndex => $registeredExtension) {
            $normalizedExtensionIdentifier = strtolower($registeredExtension->identifier);
            if (array_key_exists($normalizedExtensionIdentifier, $extensionIdentifiers)) {
                throw new InvalidArgumentException(
                    "Extension `{$registeredExtension->identifier}` is registered more than once.",
                );
            }

            $extensionIdentifiers[$normalizedExtensionIdentifier] = true;
            if ($registeredExtension->workerReducer !== null) {
                $workerReducerIndices[] = $extensionIndex;
            }

            foreach ($registeredExtension->linterRules as $rule) {
                $definition = $rule->getDefinition();
                if (array_key_exists($definition->code, $ruleCodes)) {
                    throw new InvalidArgumentException(
                        "Linter rule `{$definition->code}` is registered by more than one extension.",
                    );
                }

                $ruleCodes[$definition->code] = true;
                $ruleIndex = count($rules);
                if ($ruleIndex > 65_535) {
                    throw new InvalidArgumentException('A worker cannot register more than 65,536 linter rules.');
                }

                $rules[] = new RegisteredRule($ruleIndex, $rule, $definition);
            }

            foreach ($registeredExtension->analyzerPlugins as $plugin) {
                $definition = $plugin->getDefinition();
                foreach ([$definition->identifier, ...$definition->aliases] as $selector) {
                    $normalizedSelector = strtolower($selector);
                    if (array_key_exists($normalizedSelector, $pluginSelectors)) {
                        $first = $pluginSelectors[$normalizedSelector];
                        throw new InvalidArgumentException(
                            "Analyzer plugin selector `{$selector}` is shared by plugins `{$first}` and `{$definition->identifier}`.",
                        );
                    }

                    $pluginSelectors[$normalizedSelector] = $definition->identifier;
                }
                $registry = new AnalyzerPluginRegistry();
                $plugin->register($registry);
                $registeredFunctionProviders = self::registerTargetedCallbacks(
                    $registry->getFunctionReturnTypeProviders(),
                    $functionProviders,
                    $definition->identifier,
                    'function return-type provider',
                );
                $registeredMethodProviders = self::registerTargetedCallbacks(
                    $registry->getMethodReturnTypeProviders(),
                    $methodProviders,
                    $definition->identifier,
                    'method return-type provider',
                );
                $registeredFunctionAssertionProviders = self::registerTargetedCallbacks(
                    $registry->getFunctionAssertionProviders(),
                    $functionAssertionProviders,
                    $definition->identifier,
                    'function assertion provider',
                );
                $registeredMethodAssertionProviders = self::registerTargetedCallbacks(
                    $registry->getMethodAssertionProviders(),
                    $methodAssertionProviders,
                    $definition->identifier,
                    'method assertion provider',
                );
                $registeredPropertyProviders = self::registerTargetedCallbacks(
                    $registry->getPropertyTypeProviders(),
                    $propertyProviders,
                    $definition->identifier,
                    'property type provider',
                );
                $registeredPropertyInitializationProviders = self::registerTargetedCallbacks(
                    $registry->getPropertyInitializationProviders(),
                    $propertyInitializationProviders,
                    $definition->identifier,
                    'property initialization provider',
                );
                $registeredClassInitializerProviders = self::registerTargetedCallbacks(
                    $registry->getClassInitializerProviders(),
                    $classInitializerProviders,
                    $definition->identifier,
                    'class initializer provider',
                );
                $registeredCallForwardingProviders = self::registerTargetedCallbacks(
                    $registry->getCallForwardingProviders(),
                    $callForwardingProviders,
                    $definition->identifier,
                    'call forwarding provider',
                );

                $registeredIssueFilterHooks = [];
                foreach ($registry->getIssueFilterHooks() as $hook) {
                    $codes = $hook->getCodes();
                    if ($codes === []) {
                        throw new InvalidArgumentException(
                            "An issue-filter hook in `{$definition->identifier}` has no target codes.",
                        );
                    }
                    $seenCodes = [];
                    foreach ($codes as $code) {
                        if (array_key_exists($code, $seenCodes)) {
                            throw new InvalidArgumentException(
                                "An issue-filter hook in `{$definition->identifier}` targets issue code `{$code}` more than once.",
                            );
                        }
                        $seenCodes[$code] = true;
                    }

                    $hookIndex = count($issueFilterHooks);
                    if ($hookIndex > 65_535) {
                        throw new InvalidArgumentException(
                            'A worker cannot register more than 65,536 analyzer issue-filter hooks.',
                        );
                    }

                    $registeredHook = new RegisteredTargetedCallback(
                        $hookIndex,
                        $definition->identifier,
                        $hook,
                        $codes,
                    );
                    $issueFilterHooks[] = $registeredHook;
                    $registeredIssueFilterHooks[] = $registeredHook;
                }

                $registeredCodebaseScanHooks = self::registerTargetedCallbacks(
                    $registry->getCodebaseScanHooks(),
                    $codebaseScanHooks,
                    $definition->identifier,
                    'codebase-scan hook',
                    static function (string $pattern): string {
                        if ($pattern === '' || str_contains($pattern, "\0")) {
                            throw new InvalidArgumentException(
                                'A codebase-scan target must be non-empty and cannot contain NUL.',
                            );
                        }

                        return $pattern;
                    },
                );

                $registeredNodeAnalysisHooks = self::registerTargetedCallbacks(
                    $registry->getNodeAnalysisHooks(),
                    $nodeAnalysisHooks,
                    $definition->identifier,
                    'analyzer node-analysis hook',
                    static fn(NodeKind $target): string => $target->value,
                );
                $registeredNodeAnalysisHooksByNodeKind = [];
                foreach ($registeredNodeAnalysisHooks as $registeredHook) {
                    foreach ($registeredHook->targets as $target) {
                        $registeredNodeAnalysisHooksByNodeKind[$target->value][] = $registeredHook;
                    }
                }

                // A hook's targets name class-likes and methods as written, since the worker knows no class's rule:
                // `Box::open` and `Box::Open` are two methods of a class with exact names, and the PHP# `Demo\Order`
                // and `Demo\ORDER` two class-likes, spec section 24. Mago matches each by the codebase's keys.
                $registeredMethodCallAnalysisHooks = self::registerTargetedCallbacks(
                    $registry->getMethodCallAnalysisHooks(),
                    $methodCallAnalysisHooks,
                    $definition->identifier,
                    'analyzer method-call analysis hook',
                    static fn(MethodTarget $target): string => $target->class . '::' . $target->method,
                );
                $registeredMethodCallAnalysisHooksByIndex = [];
                foreach ($registeredMethodCallAnalysisHooks as $registeredHook) {
                    $registeredMethodCallAnalysisHooksByIndex[$registeredHook->index] = $registeredHook;
                }

                $registeredClassLikeAnalysisHooks = self::registerTargetedCallbacks(
                    $registry->getClassLikeAnalysisHooks(),
                    $classLikeAnalysisHooks,
                    $definition->identifier,
                    'analyzer class-like analysis hook',
                    static fn(ClassLikeTarget $target): string => $target->ancestor,
                );
                $registeredClassLikeAnalysisHooksByIndex = [];
                foreach ($registeredClassLikeAnalysisHooks as $registeredHook) {
                    $registeredClassLikeAnalysisHooksByIndex[$registeredHook->index] = $registeredHook;
                }

                $registeredPlugins[] = new RegisteredPlugin(
                    count($registeredPlugins),
                    $registeredExtension->identifier,
                    $plugin,
                    $definition,
                    $registeredFunctionProviders,
                    $registeredMethodProviders,
                    $registeredFunctionAssertionProviders,
                    $registeredMethodAssertionProviders,
                    $registeredPropertyProviders,
                    $registeredPropertyInitializationProviders,
                    $registeredClassInitializerProviders,
                    $registry->getEntryPoints(),
                    $registry->getAttributedEntryPoints(),
                    $registeredIssueFilterHooks,
                    $registry->getInitializationHooks(),
                    $registeredCodebaseScanHooks,
                    $registry->getBeforeAnalysisHooks(),
                    $registry->getAfterFileAnalysisHooks(),
                    $registeredNodeAnalysisHooks,
                    $registeredNodeAnalysisHooksByNodeKind,
                    $registeredMethodCallAnalysisHooks,
                    $registeredMethodCallAnalysisHooksByIndex,
                    $registeredClassLikeAnalysisHooks,
                    $registeredClassLikeAnalysisHooksByIndex,
                    $registry->getAfterAnalysisHooks(),
                    $registeredCallForwardingProviders,
                );
                if (
                    [
                        ...$registeredFunctionProviders,
                        ...$registeredMethodProviders,
                        ...$registeredFunctionAssertionProviders,
                        ...$registeredMethodAssertionProviders,
                        ...$registeredPropertyProviders,
                        ...$registeredPropertyInitializationProviders,
                        ...$registeredClassInitializerProviders,
                        ...$registeredCallForwardingProviders,
                    ] !== []
                ) {
                    $pluginsWithProviders[$definition->identifier] = true;
                }
            }
        }

        $this->extensions = $extensions;
        $this->rules = $rules;
        $this->analyzerPlugins = $registeredPlugins;
        $this->workerReducerIndices = $workerReducerIndices;
        $this->functionReturnTypeProviders = $functionProviders;
        $this->methodReturnTypeProviders = $methodProviders;
        $this->functionAssertionProviders = $functionAssertionProviders;
        $this->methodAssertionProviders = $methodAssertionProviders;
        $this->propertyTypeProviders = $propertyProviders;
        $this->propertyInitializationProviders = $propertyInitializationProviders;
        $this->classInitializerProviders = $classInitializerProviders;
        $this->callForwardingProviders = $callForwardingProviders;
        $this->issueFilterHooks = $issueFilterHooks;
        $this->codebaseScanHooks = $codebaseScanHooks;
        $this->pluginsWithProviders = $pluginsWithProviders;
    }

    /**
     * @template TTarget
     * @template TCallback of CodebaseScanHook|TargetedProvider<TTarget>|TargetedAnalysisHook<TTarget>
     *
     * @param list<TCallback> $callbacks
     * @param list<RegisteredTargetedCallback<TCallback, TTarget>> $all
     * @param non-empty-string $plugin
     * @param non-empty-string $kind
     * @param null|Closure(TTarget): non-empty-string $targetKey
     *
     * @return list<RegisteredTargetedCallback<TCallback, TTarget>>
     */
    private static function registerTargetedCallbacks(
        array $callbacks,
        array &$all,
        string $plugin,
        string $kind,
        ?Closure $targetKey = null,
    ): array {
        $registered = [];
        foreach ($callbacks as $callback) {
            $targets = $callback->getTargets();
            if ($targets === []) {
                throw new InvalidArgumentException("A {$kind} in `{$plugin}` has no targets.");
            }

            if ($targetKey !== null) {
                $seen = [];
                foreach ($targets as $target) {
                    $key = $targetKey($target);
                    if (array_key_exists($key, $seen)) {
                        throw new InvalidArgumentException("A {$kind} in `{$plugin}` targets `{$key}` more than once.");
                    }
                    $seen[$key] = true;
                }
            }

            $requirements = [];
            if ($callback instanceof TargetedAnalysisHook) {
                /** @var TargetedAnalysisHook<TTarget> $hook */
                $hook = $callback;
                $requirements = $hook->getRequirements();
                $seen = [];
                foreach ($requirements as $requirement) {
                    if (array_key_exists($requirement->name, $seen)) {
                        throw new InvalidArgumentException(
                            "A {$kind} in `{$plugin}` requests `{$requirement->name}` more than once.",
                        );
                    }
                    $seen[$requirement->name] = true;
                }
            }

            $index = count($all);
            if ($index > 65_535) {
                throw new InvalidArgumentException("A worker cannot register more than 65,536 {$kind}s.");
            }

            $entry = new RegisteredTargetedCallback($index, $plugin, $callback, $targets, $requirements);
            $all[] = $entry;
            $registered[] = $entry;
        }

        return $registered;
    }

    /**
     * Registers this worker with mago, which runs the host's file in each analysis thread's interpreter and calls the
     * worker there, one request at a time.
     *
     * @throws ProtocolException when the file runs outside mago.
     */
    public function run(): void
    {
        if (!function_exists('Mago\\Engine\\ask')) {
            throw new ProtocolException(
                'A worker runs inside mago: name its file as an extension host\'s `file` in mago.toml, and run mago.',
            );
        }

        Hosts::register($this->handleRequest(...));
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleRequest(array $payload): array
    {
        $protocol = match ($payload[0] ?? null) {
            AnalyzerProtocol::NAME => AnalyzerProtocol::class,
            LinterProtocol::NAME => LinterProtocol::class,
            WorkerProtocol::NAME => WorkerProtocol::class,
            default => throw new ProtocolException('Unknown Mago extension capability protocol.'),
        };
        try {
            [$kind, $reader] = $protocol::readRequest($payload);
        } catch (ProtocolException $exception) {
            throw $this->unreadable($exception);
        }

        return match ($protocol) {
            AnalyzerProtocol::class => $this->handleAnalyzerRequest($kind, $reader, new HostClient()),
            LinterProtocol::class => $this->handleLinterRequest($kind, $reader),
            WorkerProtocol::class => $this->handleWorkerRequest($kind, $reader, new HostClient()),
        };
    }

    /**
     * The error for a request of mago's that `$exception` says this worker can't read, which names its extensions.
     */
    private function unreadable(ProtocolException $exception): ProtocolException
    {
        $extensions = implode(', ', array_map(
            static fn(Extension $extension): string => "`{$extension->identifier}`",
            $this->extensions,
        ));

        return new ProtocolException(
            "The worker of {$extensions} can't read mago's request: {$exception->getMessage()}",
            0,
            $exception,
        );
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleWorkerRequest(int $kind, ValueReader $reader, HostClient $host): array
    {
        if ($kind === WorkerProtocol::CLASS_LOADS_REQUEST) {
            return WorkerProtocol::writeClassLoadsResponse($this->classLoads->holdsAny(WorkerProtocol::readClassLoadsRequest(
                $reader,
            )));
        }

        if ($kind === WorkerProtocol::COLLECT_REQUEST) {
            $reader->finish();
            $payloads = [];
            foreach ($this->workerReducerIndices as $extensionIndex) {
                $extension = $this->extensions[$extensionIndex];
                $host->progress("the worker reducer of extension `{$extension->identifier}`");
                try {
                    $payloads[$extensionIndex] = $extension->workerReducer?->collect() ?? '';
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Worker reducer for extension `{$extension->identifier}` failed to collect: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }
            }

            return WorkerProtocol::writeCollectResponse($payloads);
        }

        $payloadsByExtension = WorkerProtocol::readReduceRequest($reader, $this->workerReducerIndices);
        foreach ($this->workerReducerIndices as $extensionIndex) {
            $extension = $this->extensions[$extensionIndex];
            $host->progress("the worker reducer of extension `{$extension->identifier}`");
            try {
                $extension->workerReducer?->reduce(new WorkerReductionContext($payloadsByExtension[$extensionIndex]));
            } catch (Throwable $throwable) {
                throw new ProtocolException(
                    "Worker reducer for extension `{$extension->identifier}` failed to reduce: {$throwable->getMessage()}",
                    0,
                    $throwable,
                );
            }
        }

        return WorkerProtocol::writeReduceResponse();
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleLinterRequest(int $kind, ValueReader $reader): array
    {
        if ($kind === LinterProtocol::DESCRIBE_REQUEST) {
            [$this->phpVersion, $this->nodeKinds] = LinterProtocol::readDescribeRequest($reader);

            return LinterProtocol::writeDescribeResponse($this->extensions);
        }

        if ($kind !== LinterProtocol::LINT_FILE_REQUEST) {
            throw new ProtocolException("Unknown linter request kind {$kind}.");
        }

        $ruleTimes = $reader->readBoolean() ? new RuleTimes() : null;
        $request = LinterProtocol::readLintRequest($reader, $this->phpVersion, $this->nodeKinds);
        if ($request->activeRules !== $this->activeRuleIndices) {
            $activeRulesByNodeKind = [];
            foreach ($request->activeRules as $ruleIndex) {
                $registeredRule = $this->rules[$ruleIndex] ?? null;
                if ($registeredRule === null) {
                    throw new ProtocolException("Mago requested unregistered linter rule index {$ruleIndex}.");
                }

                foreach ($registeredRule->definition->targets as $target) {
                    $activeRulesByNodeKind[$target->value][] = $registeredRule;
                }
            }

            $this->activeRuleIndices = $request->activeRules;
            $this->activeRulesByNodeKind = $activeRulesByNodeKind;
        }

        $reportedIssues = [];
        foreach ($request->file->getTargetNodes() as $node) {
            $context = new LintContext($request->file, $node);
            foreach ($this->activeRulesByNodeKind[$node->kind->value] ?? [] as $registeredRule) {
                try {
                    $ruleTimes?->start();
                    $registeredRule->rule->lint($context);
                    $ruleTimes?->stop($registeredRule->rule, 'lint', null);
                } catch (Throwable $throwable) {
                    $code = $registeredRule->definition->code;
                    throw new ProtocolException(
                        "Linter rule `{$code}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }

                foreach ($context->issues as $issue) {
                    $reportedIssues[] = $registeredRule->index;
                    $reportedIssues[] = $issue;
                }

                $context->issues = [];
            }
        }

        return LinterProtocol::writeLintResponse($reportedIssues, $ruleTimes);
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleAnalyzerRequest(int $kind, ValueReader $reader, HostClient $host): array
    {
        if ($kind === AnalyzerProtocol::DESCRIBE_REQUEST) {
            [$this->phpVersion, $this->nodeKinds] = AnalyzerProtocol::readDescribeRequest($reader);

            return AnalyzerProtocol::writeDescribeResponse($this->extensions, $this->analyzerPlugins);
        }

        if ($kind === AnalyzerProtocol::INITIALIZE_REQUEST) {
            return $this->handleAnalyzerInitialization($reader, $host);
        }

        if ($kind === AnalyzerProtocol::CODEBASE_SCAN_REQUEST) {
            return $this->handleCodebaseScanRequest($reader, $host);
        }

        $ruleTimes = $reader->readBoolean() ? new RuleTimes() : null;
        if (
            $kind === AnalyzerProtocol::BEFORE_ANALYSIS_REQUEST
            || $kind === AnalyzerProtocol::AFTER_FILE_ANALYSIS_REQUEST
            || $kind === AnalyzerProtocol::AFTER_ANALYSIS_REQUEST
            || $kind === AnalyzerProtocol::AFTER_FILE_ANALYSIS_BATCH_REQUEST
        ) {
            return $this->handleAnalyzerLifecycleRequest($kind, $reader, $host, $ruleTimes);
        }

        $issueReads = new CodebaseReads();
        $codeReads = new CodebaseReads();
        $response = $this->handleProviderRequest($kind, $reader, $host, $issueReads, $codeReads, $ruleTimes);

        return AnalyzerProtocol::withCodebaseReads(
            $response,
            $this->sentReads,
            (
                $this->metadataCache ?? throw new ProtocolException('A provider request named no analysis generation.')
            )->generation,
            $issueReads,
            $codeReads,
            $this->classLoads,
            $ruleTimes,
        );
    }

    /**
     * Answers a provider or issue-filter request, recording every codebase read it makes for the file Mago is
     * analyzing: in `$codeReads` when the plugin that made it registers a provider, as every provider's plugin does,
     * and in `$issueReads` otherwise.
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleProviderRequest(
        int $kind,
        ValueReader $reader,
        HostClient $host,
        CodebaseReads $issueReads,
        CodebaseReads $codeReads,
        ?RuleTimes $ruleTimes,
    ): array {
        if ($kind === AnalyzerProtocol::PROPERTY_TYPE_REQUEST) {
            $request = AnalyzerProtocol::readPropertyTypeRequest($reader);
            if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
                $this->metadataCache = new MetadataCache($request->generation);
            }

            $codebase = new Codebase($host, $this->metadataCache, $codeReads);
            $types = new TypeComparator($host, $this->metadataCache, $codeReads);
            foreach ($request->providerIndices as $providerIndex) {
                $registered = $this->propertyTypeProviders[$providerIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException(
                        "Mago requested unregistered property type provider index {$providerIndex}.",
                    );
                }

                try {
                    $ruleTimes?->start();
                    $type = $registered->callback->getPropertyType(
                        new PropertyTypeProviderContext($this->phpVersion, $codebase, $request->access, $types),
                    );
                    $ruleTimes?->stop($registered->callback, 'getPropertyType', null);
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Property type provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }

                if ($type !== null) {
                    return AnalyzerProtocol::writePropertyTypeResponse($type);
                }
            }

            return AnalyzerProtocol::writePropertyTypeResponse(null);
        }

        if ($kind === AnalyzerProtocol::CALL_FORWARDING_REQUEST) {
            $request = AnalyzerProtocol::readCallForwardingRequest($reader);
            if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
                $this->metadataCache = new MetadataCache($request->generation);
            }

            $context = new CallForwardingProviderContext(
                $this->phpVersion,
                new Codebase($host, $this->metadataCache, $codeReads),
                $request->class,
                $request->member,
                $request->property,
                $request->receiverType,
                new TypeComparator($host, $this->metadataCache, $codeReads),
            );
            foreach ($request->providerIndices as $providerIndex) {
                $registered = $this->callForwardingProviders[$providerIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException(
                        "Mago requested unregistered call forwarding provider index {$providerIndex}.",
                    );
                }

                try {
                    $ruleTimes?->start();
                    $forwarded = $registered->callback->getForwardedCall($context);
                    $ruleTimes?->stop($registered->callback, 'getForwardedCall', null);
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Call forwarding provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }

                if ($forwarded !== null) {
                    return AnalyzerProtocol::writeCallForwardingResponse($forwarded);
                }
            }

            return AnalyzerProtocol::writeCallForwardingResponse(null);
        }

        if ($kind === AnalyzerProtocol::PROPERTY_INITIALIZATION_REQUEST) {
            $request = AnalyzerProtocol::readPropertyInitializationRequest($reader);
            if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
                $this->metadataCache = new MetadataCache($request->generation);
            }

            $codebase = new Codebase($host, $this->metadataCache, $codeReads);
            $types = new TypeComparator($host, $this->metadataCache, $codeReads);
            foreach ($request->providerIndices as $providerIndex) {
                $registered = $this->propertyInitializationProviders[$providerIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException(
                        "Mago requested unregistered property initialization provider index {$providerIndex}.",
                    );
                }

                try {
                    $ruleTimes?->start();
                    $initialized = $registered->callback->isPropertyInitialized(
                        new PropertyInitializationProviderContext(
                            $this->phpVersion,
                            $codebase,
                            $request->declaringClass,
                            $request->property,
                            $types,
                        ),
                    );
                    $ruleTimes?->stop($registered->callback, 'isPropertyInitialized', null);
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Property initialization provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }

                if ($initialized) {
                    return AnalyzerProtocol::writePropertyInitializationResponse(true);
                }
            }

            return AnalyzerProtocol::writePropertyInitializationResponse(false);
        }

        if ($kind === AnalyzerProtocol::CLASS_INITIALIZER_REQUEST) {
            $request = AnalyzerProtocol::readClassInitializerRequest($reader);
            if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
                $this->metadataCache = new MetadataCache($request->generation);
            }

            $codebase = new Codebase($host, $this->metadataCache, $codeReads);
            $initializers = [];
            foreach ($request->providerIndices as $providerIndex) {
                $registered = $this->classInitializerProviders[$providerIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException(
                        "Mago requested unregistered class initializer provider index {$providerIndex}.",
                    );
                }

                try {
                    $ruleTimes?->start();
                    $provided = $registered->callback->getClassInitializers(
                        new ClassInitializerProviderContext($this->phpVersion, $codebase, $request->class),
                    );
                    $ruleTimes?->stop($registered->callback, 'getClassInitializers', null);
                    foreach ($provided as $initializer) {
                        DefinitionName::assertIdentifier($initializer, 'A class initializer method name');
                        $initializers[$initializer] = $initializer;
                    }
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Class initializer provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }
            }

            return AnalyzerProtocol::writeClassInitializerResponse(array_values($initializers));
        }

        if ($kind === AnalyzerProtocol::ISSUE_FILTER_REQUEST) {
            $request = AnalyzerProtocol::readIssueFilterRequest($reader);
            if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
                $this->metadataCache = new MetadataCache($request->generation);
            }

            $issueCodebase = new Codebase($host, $this->metadataCache, $issueReads);
            $issueTypes = new TypeComparator($host, $this->metadataCache, $issueReads);
            $codeCodebase = new Codebase($host, $this->metadataCache, $codeReads);
            $codeTypes = new TypeComparator($host, $this->metadataCache, $codeReads);
            $removed = [];
            foreach ($request->issues as $issueIndex => $issue) {
                foreach ($request->hookIndices as $hookIndex) {
                    $registered = $this->issueFilterHooks[$hookIndex] ?? null;
                    if ($registered === null) {
                        throw new ProtocolException(
                            "Mago requested unregistered analyzer issue-filter hook index {$hookIndex}.",
                        );
                    }

                    if ($issue->code === null || !in_array($issue->code, $registered->targets, true)) {
                        continue;
                    }

                    $providing = array_key_exists($registered->plugin, $this->pluginsWithProviders);
                    $context = new IssueFilterContext(
                        $this->phpVersion,
                        $providing ? $codeCodebase : $issueCodebase,
                        $providing ? $codeTypes : $issueTypes,
                        $request->file,
                        $request->contents,
                        $issue,
                    );
                    try {
                        $ruleTimes?->start();
                        $decision = $registered->callback->filterIssue($context);
                        $ruleTimes?->stop($registered->callback, 'filterIssue', null);
                    } catch (Throwable $throwable) {
                        throw new ProtocolException(
                            "Analyzer issue-filter hook in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                            0,
                            $throwable,
                        );
                    }

                    if ($decision === IssueFilterDecision::Remove) {
                        $removed[] = $issueIndex;
                        break;
                    }
                }
            }

            return AnalyzerProtocol::writeIssueFilterResponse(count($request->issues), $removed);
        }

        if (
            $kind !== AnalyzerProtocol::RETURN_TYPE_REQUEST
            && $kind !== AnalyzerProtocol::CALLABLE_SIGNATURE_REQUEST
            && $kind !== AnalyzerProtocol::ASSERTION_REQUEST
        ) {
            throw new ProtocolException("Unknown analyzer request kind {$kind}.");
        }

        $request = AnalyzerProtocol::readReturnTypeRequest($reader);
        if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
            $this->metadataCache = new MetadataCache($request->generation);
        }
        $codebase = new Codebase($host, $this->metadataCache, $codeReads);
        $types = new TypeComparator($host, $this->metadataCache, $codeReads);
        if ($kind === AnalyzerProtocol::ASSERTION_REQUEST) {
            $providers = $request->invocation->kind === InvocationKind::Function
                ? $this->functionAssertionProviders
                : $this->methodAssertionProviders;
            $context = new AssertionProviderContext($this->phpVersion, $codebase, $request->invocation, $types);
            foreach ($request->providerIndices as $providerIndex) {
                $registered = $providers[$providerIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException(
                        "Mago requested unregistered analyzer assertion provider index {$providerIndex}.",
                    );
                }

                try {
                    $ruleTimes?->start();
                    $assertions = $registered->callback->getAssertions($context);
                    $ruleTimes?->stop($registered->callback, 'getAssertions', null);
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Analyzer assertion provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }

                if ($assertions !== null && !$assertions->isEmpty()) {
                    return AnalyzerProtocol::writeAssertionResponse($assertions);
                }
            }

            return AnalyzerProtocol::writeAssertionResponse(null);
        }

        $providers = $request->invocation->kind === InvocationKind::Function
            ? $this->functionReturnTypeProviders
            : $this->methodReturnTypeProviders;
        if ($kind === AnalyzerProtocol::CALLABLE_SIGNATURE_REQUEST) {
            $context = new CallableSignatureProviderContext($this->phpVersion, $codebase, $request->invocation, $types);
            foreach ($request->providerIndices as $providerIndex) {
                $registered = $providers[$providerIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException(
                        "Mago requested unregistered analyzer provider index {$providerIndex}.",
                    );
                }

                try {
                    if (!$registered->callback instanceof CallableSignatureProvider) {
                        throw new ProtocolException(
                            "Mago requested a callable signature from analyzer provider {$providerIndex}, but it does not advertise that capability.",
                        );
                    }

                    $ruleTimes?->start();
                    $signature = $registered->callback->getCallableSignature($context);
                    $ruleTimes?->stop($registered->callback, 'getCallableSignature', null);
                    if ($signature !== null) {
                        return AnalyzerProtocol::writeCallableSignatureResponse($signature);
                    }
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Analyzer provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }
            }

            return AnalyzerProtocol::writeCallableSignatureResponse(null);
        }

        $context = new ReturnTypeProviderContext($this->phpVersion, $codebase, $request->invocation, $types);
        foreach ($request->providerIndices as $providerIndex) {
            $registered = $providers[$providerIndex] ?? null;
            if ($registered === null) {
                throw new ProtocolException("Mago requested unregistered analyzer provider index {$providerIndex}.");
            }

            try {
                $ruleTimes?->start();
                $type = $registered->callback->getReturnType($context);
                $ruleTimes?->stop($registered->callback, 'getReturnType', null);
            } catch (Throwable $throwable) {
                throw new ProtocolException(
                    "Analyzer provider in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                    0,
                    $throwable,
                );
            }

            if ($type !== null) {
                return AnalyzerProtocol::writeReturnTypeResponse($type);
            }
        }

        return AnalyzerProtocol::writeReturnTypeResponse(null);
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleAnalyzerInitialization(ValueReader $reader, HostClient $host): array
    {
        $pluginIndices = AnalyzerProtocol::readInitializationRequest($reader);
        $stubs = [];
        foreach ($pluginIndices as $pluginIndex) {
            $registered = $this->analyzerPlugins[$pluginIndex] ?? null;
            if ($registered === null) {
                throw new ProtocolException("Mago initialized unregistered analyzer plugin index {$pluginIndex}.");
            }

            $host->progress("the initialization hooks of plugin `{$registered->definition->identifier}`");
            $context = new InitializationContext($this->phpVersion);
            try {
                foreach ($registered->initializationHooks as $hook) {
                    $hook->initialize($context);
                }
            } catch (Throwable $throwable) {
                throw new ProtocolException(
                    "Analyzer plugin `{$registered->definition->identifier}` failed to initialize: {$throwable->getMessage()}",
                    0,
                    $throwable,
                );
            }

            $stubs[$pluginIndex] = $context->getStubs();
        }

        return AnalyzerProtocol::writeInitializationResponse($stubs);
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleCodebaseScanRequest(ValueReader $reader, HostClient $host): array
    {
        [$firstBatch, $lastBatch, $activeHooks, $filesByHook] = AnalyzerProtocol::readCodebaseScanRequest($reader);
        $refinements = [];
        foreach ($activeHooks as $hookIndex) {
            $registered = $this->codebaseScanHooks[$hookIndex] ?? null;
            if ($registered === null) {
                throw new ProtocolException("Codebase-scan request targets unknown hook {$hookIndex}.");
            }
            $host->progress("a codebase-scan hook of plugin `{$registered->plugin}`");
            $context = new CodebaseScanContext(
                $this->phpVersion,
                $filesByHook[$registered->index] ?? [],
                $firstBatch,
                $lastBatch,
            );
            try {
                $registered->callback->scan($context);
            } catch (Throwable $throwable) {
                throw new ProtocolException(
                    "Codebase-scan hook in `{$registered->plugin}` failed: {$throwable->getMessage()}",
                    0,
                    $throwable,
                );
            }

            foreach ($context->getRefinements() as $refinement) {
                $refinements[] = [$registered->index, $refinement];
            }
        }

        return AnalyzerProtocol::writeCodebaseScanResponse($refinements);
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private function handleAnalyzerLifecycleRequest(
        int $kind,
        ValueReader $reader,
        HostClient $host,
        ?RuleTimes $ruleTimes,
    ): array {
        $request = AnalyzerProtocol::readLifecycleRequest($kind, $reader, $host, $this->phpVersion, $this->nodeKinds);
        if ($this->metadataCache === null || $this->metadataCache->generation !== $request->generation) {
            $this->metadataCache = new MetadataCache($request->generation);
        }
        $codebase = new Codebase($host, $this->metadataCache);
        $types = new TypeComparator($host, $this->metadataCache);
        $reportedIssues = [];
        $contributedReferences = [];
        $recordedReads = [];
        $analyses = is_array($request->analysis) ? $request->analysis : [$request->analysis];
        $projectHooks = match ($kind) {
            AnalyzerProtocol::BEFORE_ANALYSIS_REQUEST => 'before-analysis',
            AnalyzerProtocol::AFTER_ANALYSIS_REQUEST => 'after-analysis',
            default => null,
        };
        foreach ($analyses as $analysis) {
            // A batch runs several files' hooks, so each file's start is progress of its own. Mago names the one file
            // of an after-file request itself.
            if ($analysis instanceof FileAnalysis && $kind === AnalyzerProtocol::AFTER_FILE_ANALYSIS_BATCH_REQUEST) {
                $host->progress("the hooks of {$analysis->file}");
            }

            // A file's hooks read for it, and the before-analysis hooks for every file, since a rule may keep what it
            // read there and answer each file from it. What a plugin with a provider reads can change the code.
            $issueReads = new CodebaseReads();
            $codeReads = new CodebaseReads();
            $recording = $kind !== AnalyzerProtocol::AFTER_ANALYSIS_REQUEST;
            $issueCodebase = new Codebase($host, $this->metadataCache, $issueReads);
            $issueTypes = new TypeComparator($host, $this->metadataCache, $issueReads);
            $codeCodebase = new Codebase($host, $this->metadataCache, $codeReads);
            $codeTypes = new TypeComparator($host, $this->metadataCache, $codeReads);
            foreach ($request->pluginIndices as $pluginIndex) {
                $registered = $this->analyzerPlugins[$pluginIndex] ?? null;
                if ($registered === null) {
                    throw new ProtocolException("Mago requested unregistered analyzer plugin index {$pluginIndex}.");
                }

                $identifier = $registered->definition->identifier;
                // One request runs every plugin's project-level hooks, so each plugin's start is progress of its own.
                if ($projectHooks !== null) {
                    $host->progress("the {$projectHooks} hooks of plugin `{$identifier}`");
                }

                $providing = array_key_exists($identifier, $this->pluginsWithProviders);
                $fileCodebase = match (true) {
                    !$recording => $codebase,
                    $providing => $codeCodebase,
                    default => $issueCodebase,
                };
                $fileTypes = match (true) {
                    !$recording => $types,
                    $providing => $codeTypes,
                    default => $issueTypes,
                };

                try {
                    $context = match ($kind) {
                        AnalyzerProtocol::BEFORE_ANALYSIS_REQUEST => new BeforeAnalysisContext(
                            $this->phpVersion,
                            $fileCodebase,
                            $fileTypes,
                        ),
                        AnalyzerProtocol::AFTER_FILE_ANALYSIS_REQUEST => new AfterFileAnalysisContext(
                            $this->phpVersion,
                            $fileCodebase,
                            $fileTypes,
                            $analysis instanceof FileAnalysis
                                ? $analysis
                                : throw new ProtocolException('An after-file request has no file analysis.'),
                        ),
                        AnalyzerProtocol::AFTER_FILE_ANALYSIS_BATCH_REQUEST => new AfterFileAnalysisContext(
                            $this->phpVersion,
                            $fileCodebase,
                            $fileTypes,
                            $analysis instanceof FileAnalysis
                                ? $analysis
                                : throw new ProtocolException('An after-file batch contains an invalid analysis.'),
                        ),
                        AnalyzerProtocol::AFTER_ANALYSIS_REQUEST => new AfterAnalysisContext(
                            $this->phpVersion,
                            $codebase,
                            $types,
                            $analysis instanceof ProjectAnalysis
                                ? $analysis
                                : throw new ProtocolException('An after-analysis request has no project analysis.'),
                        ),
                        default => throw new ProtocolException("Unknown analyzer lifecycle request kind {$kind}."),
                    };

                    if ($context instanceof BeforeAnalysisContext) {
                        foreach ($registered->beforeAnalysisHooks as $hook) {
                            $ruleTimes?->start();
                            $hook->beforeAnalysis($context);
                            $ruleTimes?->stop($hook, 'beforeAnalysis', null);
                        }
                    }

                    if ($context instanceof AfterFileAnalysisContext) {
                        foreach ($registered->afterFileAnalysisHooks as $hook) {
                            $ruleTimes?->start();
                            $hook->afterFileAnalysis($context);
                            $ruleTimes?->stop($hook, 'afterFileAnalysis', $context->analysis->file);
                        }

                        $this->runNodeAnalysisHooks($registered, $context, $pluginIndex, $reportedIssues, $ruleTimes);
                    }

                    if ($context instanceof AfterAnalysisContext) {
                        foreach ($registered->afterAnalysisHooks as $hook) {
                            $ruleTimes?->start();
                            $hook->afterAnalysis($context);
                            $ruleTimes?->stop($hook, 'afterAnalysis', null);
                        }
                    }
                } catch (Throwable $throwable) {
                    throw new ProtocolException(
                        "Analyzer lifecycle hook in `{$identifier}` failed: {$throwable->getMessage()}",
                        0,
                        $throwable,
                    );
                }

                foreach ($context->takeReportedIssues() as $issue) {
                    $reportedIssues[] = $pluginIndex;
                    $reportedIssues[] = $issue;
                    $reportedIssues[] = $context instanceof AfterFileAnalysisContext ? $context->analysis->file : null;
                }

                if ($context instanceof BeforeAnalysisContext || $context instanceof AfterFileAnalysisContext) {
                    $references = $context->references->takeReferences();
                    for ($index = 0, $count = count($references); $index < $count; $index += 3) {
                        $contributedReferences[] = $pluginIndex;
                        $contributedReferences[] = $references[$index];
                        $contributedReferences[] = $references[$index + 1];
                        $contributedReferences[] = $references[$index + 2];
                        $contributedReferences[] = $context instanceof AfterFileAnalysisContext
                            ? $context->analysis->file
                            : null;
                    }
                }
            }

            // A file's hooks read every class their worker had loaded when they finished, so each file names its end.
            if ($recording) {
                $recordedReads[] = [
                    $analysis instanceof FileAnalysis ? $analysis->file : null,
                    $issueReads,
                    $codeReads,
                    $this->classLoads->end(),
                ];
            }
        }

        return AnalyzerProtocol::writeLifecycleResponse(
            $kind,
            $this->classLoads,
            $this->sentReads,
            $request->generation,
            $reportedIssues,
            $contributedReferences,
            $recordedReads,
            $ruleTimes,
        );
    }

    /**
     * @param int<0, 65535> $pluginIndex
     * @param list<int|ReportedIssue|string|null> $reportedIssues
     */
    private function runNodeAnalysisHooks(
        RegisteredPlugin $registered,
        AfterFileAnalysisContext $context,
        int $pluginIndex,
        array &$reportedIssues,
        ?RuleTimes $ruleTimes,
    ): void {
        if (
            $registered->nodeAnalysisHooks === []
            && $registered->methodCallAnalysisHooks === []
            && $registered->classLikeAnalysisHooks === []
            || !$context->analysis->hasNodeAnalysisTargets()
        ) {
            return;
        }

        $source = $context->analysis->getNodeSourceFile();
        $targetIndex = 0;
        foreach ($source->getTargetNodes() as $node) {
            $data = $context->analysis->getNodeAnalysisData($targetIndex++);
            // A PHP# method call reaches the hooks of the node it means, as Mago reads it: a hook that targets function
            // calls receives `global.max()`, as it receives `max()` in PHP, one that targets static calls receives
            // `Calc.make()`, whose receiver Mago bound to a class, as it receives `Calc::make()`, and one that targets
            // instantiations receives `PaymentResult.Declined(…)`, as it receives `new PaymentResult\Declined(…)`.
            $hooks = match ($data->meaning) {
                null => $registered->nodeAnalysisHooksByNodeKind[$node->kind->value] ?? [],
                NodeKind::StaticMethodCall => self::nodeAnalysisHooksTargeting(
                    $registered,
                    NodeKind::MethodCall,
                    NodeKind::StaticMethodCall,
                ),
                default => $registered->nodeAnalysisHooksByNodeKind[$data->meaning->value] ?? [],
            };
            if ($hooks === [] && $data->targetedHookIndices === []) {
                continue;
            }

            $nodeContext = new NodeAnalysisContext($context, $source, $node, $data);
            foreach ($hooks as $hook) {
                $ruleTimes?->start();
                $hook->callback->analyze($nodeContext);
                $ruleTimes?->stop($hook->callback, 'analyze', $context->analysis->file);
                foreach ($nodeContext->takeReportedIssues() as $issue) {
                    $reportedIssues[] = $pluginIndex;
                    $reportedIssues[] = $issue;
                    $reportedIssues[] = $context->analysis->file;
                }
            }

            $targetedHooks = match ($node->kind) {
                NodeKind::Class_,
                NodeKind::Enum,
                NodeKind::Interface,
                NodeKind::Trait,
                    => $registered->classLikeAnalysisHooksByIndex,
                NodeKind::MethodCall,
                NodeKind::NullSafeMethodCall,
                NodeKind::StaticMethodCall,
                    => $registered->methodCallAnalysisHooksByIndex,
                default => [],
            };
            foreach ($data->targetedHookIndices as $hookIndex) {
                $hook = $targetedHooks[$hookIndex] ?? null;
                if ($hook === null) {
                    continue;
                }

                $ruleTimes?->start();
                $hook->callback->analyze($nodeContext);
                $ruleTimes?->stop($hook->callback, 'analyze', $context->analysis->file);
                foreach ($nodeContext->takeReportedIssues() as $issue) {
                    $reportedIssues[] = $pluginIndex;
                    $reportedIssues[] = $issue;
                    $reportedIssues[] = $context->analysis->file;
                }
            }
        }
    }

    /**
     * The node analysis hooks that target any of `$kinds`, each once, in registration order.
     *
     * @return list<RegisteredTargetedCallback<NodeAnalysisHook, NodeKind>>
     */
    private static function nodeAnalysisHooksTargeting(RegisteredPlugin $registered, NodeKind ...$kinds): array
    {
        $hooks = [];
        foreach ($registered->nodeAnalysisHooks as $hook) {
            foreach ($kinds as $kind) {
                if (!in_array($kind, $hook->targets, true)) {
                    continue;
                }

                $hooks[] = $hook;
                break;
            }
        }

        return $hooks;
    }
}
