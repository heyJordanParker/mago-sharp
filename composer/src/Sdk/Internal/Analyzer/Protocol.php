<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use Mago\Sdk\Analyzer\CallableSignatureOverride;
use Mago\Sdk\Analyzer\CallableSignatureProvider;
use Mago\Sdk\Analyzer\ClassLikeTarget;
use Mago\Sdk\Analyzer\ClassTarget;
use Mago\Sdk\Analyzer\CodebaseScanFile;
use Mago\Sdk\Analyzer\Declaration\ClassLikeRefinement;
use Mago\Sdk\Analyzer\Declaration\FunctionLikeRefinement;
use Mago\Sdk\Analyzer\Declaration\RefinedType;
use Mago\Sdk\Analyzer\Declaration\SignatureRefinement;
use Mago\Sdk\Analyzer\Declaration\TypeParameter;
use Mago\Sdk\Analyzer\EffectiveCallableSignature;
use Mago\Sdk\Analyzer\ExpressionType;
use Mago\Sdk\Analyzer\FileAnalysis;
use Mago\Sdk\Analyzer\FileAnalysisRequirement;
use Mago\Sdk\Analyzer\ForwardedCall;
use Mago\Sdk\Analyzer\FunctionTarget;
use Mago\Sdk\Analyzer\InvocationAssertions;
use Mago\Sdk\Analyzer\Metadata\MemberIdentifier;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\ProjectAnalysis;
use Mago\Sdk\Analyzer\PropertyTarget;
use Mago\Sdk\Analyzer\PropertyType;
use Mago\Sdk\Analyzer\ReferenceKind;
use Mago\Sdk\Analyzer\ReferenceOrigin;
use Mago\Sdk\Analyzer\ReferenceSummary;
use Mago\Sdk\Analyzer\SymbolReference;
use Mago\Sdk\Analyzer\SymbolReferences;
use Mago\Sdk\Analyzer\TargetedAnalysisHook;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Analyzer\Type\FunctionLikeIdentifier;
use Mago\Sdk\Analyzer\Type\Variance;
use Mago\Sdk\Analyzer\TypeComparison;
use Mago\Sdk\Analyzer\UndeclaredReturnTypeProvider;
use Mago\Sdk\Analyzer\VariableDefinedness;
use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Extension;
use Mago\Sdk\Internal\ClassLoads;
use Mago\Sdk\Internal\HostClient;
use Mago\Sdk\Internal\Protocol\PayloadWriter;
use Mago\Sdk\Internal\Protocol\ValueReader;
use Mago\Sdk\Internal\Protocol\ValueWriter;
use Mago\Sdk\Internal\Syntax\SourceFileCodec;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Reporting\Annotation;
use Mago\Sdk\Reporting\AnnotationKind;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\Reporting\ReportedIssue as ReportingReportedIssue;
use Mago\Sdk\Reporting\Safety;
use Mago\Sdk\Reporting\TextEdit;
use Mago\Sdk\Span;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Syntax\SourceFile;

use function array_key_exists;
use function array_slice;
use function count;
use function intdiv;
use function is_string;
use function strlen;

/**
 * @internal
 */
final class Protocol
{
    public const NAME = 'analyzer';

    public const DESCRIBE_REQUEST = 1;
    public const RETURN_TYPE_REQUEST = 2;
    public const TYPE_COMPARISON_REQUEST = 3;
    public const CODEBASE_QUERY_REQUEST = 4;
    public const BEFORE_ANALYSIS_REQUEST = 5;
    public const AFTER_FILE_ANALYSIS_REQUEST = 6;
    public const AFTER_ANALYSIS_REQUEST = 7;
    public const ANALYSIS_QUERY_REQUEST = 8;
    public const AFTER_FILE_ANALYSIS_BATCH_REQUEST = 9;
    public const SYMBOL_REFERENCE_QUERY_REQUEST = 10;
    public const INITIALIZE_REQUEST = 11;
    public const CALLABLE_SIGNATURE_REQUEST = 12;
    public const PROPERTY_TYPE_REQUEST = 13;
    public const PROPERTY_INITIALIZATION_REQUEST = 14;
    public const ISSUE_FILTER_REQUEST = 15;
    public const CLASS_INITIALIZER_REQUEST = 17;
    public const ASSERTION_REQUEST = 18;
    public const CODEBASE_SCAN_REQUEST = 19;
    public const CALL_FORWARDING_REQUEST = 20;
    public const GET_EXPRESSION_TYPES = 1;
    public const GET_ALL_EXPRESSION_TYPES = 2;
    public const GET_INFERRED_RETURN_TYPES = 3;
    public const GET_INFERRED_YIELD_KEY_TYPES = 4;
    public const GET_INFERRED_YIELD_VALUE_TYPES = 5;
    public const GET_SOURCE_FILE = 6;
    public const GET_REFERENCES_TO = 1;
    public const GET_REFERENCES_FROM = 2;
    public const GET_CLASS_LIKES = 1;
    public const GET_FUNCTIONS = 2;
    public const GET_METHODS = 3;
    public const GET_CONSTANTS = 4;
    public const GET_PROPERTIES = 5;
    public const GET_CLASS_CONSTANTS = 6;
    public const GET_ENUM_CASES = 7;
    public const LIST_CLASS_LIKES = 8;
    public const LIST_FUNCTIONS = 9;
    public const LIST_CONSTANTS = 10;
    public const GET_DECLARING_METHODS = 11;
    public const GET_DECLARING_PROPERTIES = 12;
    public const CHECK_EXISTENCE = 13;
    public const CHECK_MEMBER_EXISTENCE = 14;
    public const GET_CLASS_LIKE_RELATIONS = 15;
    public const GET_MAGIC_PROPERTIES = 16;
    public const GET_DECLARING_MAGIC_PROPERTIES = 17;
    public const GET_FUNCTION_LIKES = 18;
    public const FIND_METHODS = 19;
    public const EXISTS_CLASS = 1;
    public const EXISTS_INTERFACE = 2;
    public const EXISTS_TRAIT = 3;
    public const EXISTS_ENUM = 4;
    public const EXISTS_CLASS_LIKE = 5;
    public const EXISTS_NAMESPACE = 6;
    public const EXISTS_FUNCTION = 7;
    public const EXISTS_CONSTANT = 8;
    public const EXISTS_CLASS_OR_TRAIT = 9;
    public const EXISTS_CLASS_OR_INTERFACE = 10;
    public const EXISTS_METHOD = 1;
    public const EXISTS_PROPERTY = 2;
    public const EXISTS_CLASS_CONSTANT = 3;
    public const EXISTS_ENUM_CASE = 4;
    public const EXISTS_MAGIC_PROPERTY = 5;
    public const DIRECT_DESCENDANTS = 1;
    public const ALL_DESCENDANTS = 2;
    public const ALL_ANCESTORS = 3;
    public const ANY_CLASS_LIKE = 0;
    public const CLASS_LIKE_CLASS = 1;
    public const CLASS_LIKE_INTERFACE = 2;
    public const CLASS_LIKE_TRAIT = 3;
    public const CLASS_LIKE_ENUM = 4;
    private const TYPE_COMPARISON_BATCH_REQUEST = 16;
    private const DESCRIBE_RESPONSE = 0x8001;
    private const RETURN_TYPE_RESPONSE = 0x8002;
    private const TYPE_COMPARISON_RESPONSE = 0x8003;
    private const CODEBASE_QUERY_RESPONSE = 0x8004;
    private const BEFORE_ANALYSIS_RESPONSE = 0x8005;
    private const AFTER_FILE_ANALYSIS_RESPONSE = 0x8006;
    private const AFTER_ANALYSIS_RESPONSE = 0x8007;
    private const ANALYSIS_QUERY_RESPONSE = 0x8008;
    private const AFTER_FILE_ANALYSIS_BATCH_RESPONSE = 0x8009;
    private const SYMBOL_REFERENCE_QUERY_RESPONSE = 0x800A;
    private const INITIALIZE_RESPONSE = 0x800B;
    private const CALLABLE_SIGNATURE_RESPONSE = 0x800C;
    private const PROPERTY_TYPE_RESPONSE = 0x800D;
    private const PROPERTY_INITIALIZATION_RESPONSE = 0x800E;
    private const ISSUE_FILTER_RESPONSE = 0x800F;
    private const TYPE_COMPARISON_BATCH_RESPONSE = 0x8010;
    private const CLASS_INITIALIZER_RESPONSE = 0x8011;
    private const ASSERTION_RESPONSE = 0x8012;
    private const CODEBASE_SCAN_RESPONSE = 0x8013;
    private const CALL_FORWARDING_RESPONSE = 0x8014;
    private const MAXIMUM_ISSUES = 1_000_000;
    private const MAXIMUM_ISSUE_NOTES = 0x0001_0000;
    private const MAXIMUM_ISSUE_ANNOTATIONS = 0x0001_0000;
    private const MAXIMUM_ISSUE_EDITS = 0x0001_0000;
    private const METHOD_SEARCH_ANY_CLASS = 0;
    private const METHOD_SEARCH_EXACT_CLASS = 1;
    private const METHOD_SEARCH_DESCENDANTS = 2;
    private const REGISTRATION_CAPABILITIES = 1;

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{int<0, 65535>, ValueReader}
     */
    public static function readRequest(array $payload): array
    {
        $reader = new ValueReader($payload);
        if ($reader->readBytes() !== self::NAME) {
            throw new ProtocolException('The message is no analyzer message.');
        }

        return [$reader->readU16(), $reader];
    }

    /**
     * @return array{PHPVersion, list<NodeKind>}
     */
    public static function readDescribeRequest(ValueReader $reader): array
    {
        $version = new PHPVersion($reader->readU32());
        $kinds = SourceFileCodec::readNodeKinds($reader);
        $reader->finish();

        return [$version, $kinds];
    }

    /** @return list<int<0, 65535>> */
    public static function readInitializationRequest(ValueReader $reader): array
    {
        $count = $reader->readCount(65_536);
        $plugins = [];
        for ($index = 0; $index < $count; ++$index) {
            $plugins[] = $reader->readU16();
        }
        $reader->finish();

        return $plugins;
    }

    /**
     * @param array<int<0, 65535>, list<array{non-empty-string, string}>> $stubs
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeInitializationResponse(array $stubs): array
    {
        $writer = self::createMessage(self::INITIALIZE_RESPONSE);
        $writer->writeCount($stubs);
        foreach ($stubs as $plugin => $pluginStubs) {
            $writer->writeU16($plugin);
            $writer->writeCount($pluginStubs);
            foreach ($pluginStubs as [$filename, $bytes]) {
                $writer->writeBytes($filename);
                $writer->writeBytes($bytes);
            }
        }

        return $writer->finish();
    }

    /**
     * @param non-empty-list<Extension> $extensions
     * @param list<RegisteredPlugin> $plugins
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeDescribeResponse(array $extensions, array $plugins): array
    {
        $byExtension = [];
        foreach ($plugins as $plugin) {
            $byExtension[$plugin->extension][] = $plugin;
        }

        $writer = self::createMessage(self::DESCRIBE_RESPONSE);
        $writer->writeCount($extensions);
        foreach ($extensions as $extension) {
            $writer->writeString($extension->identifier);
            $writer->writeString($extension->name);
            $writer->writeString($extension->version);
            $writer->writeBoolean($extension->workerReducer !== null);
            $extensionPlugins = $byExtension[$extension->identifier] ?? [];
            $writer->writeCount($extensionPlugins);
            foreach ($extensionPlugins as $plugin) {
                $definition = $plugin->definition;
                $writer->writeString($definition->identifier);
                $writer->writeString($definition->name);
                $writer->writeString($definition->description);
                $writer->writeBoolean($definition->defaultEnabled);
                $flags = 0;
                $flags |= (int) ($plugin->beforeAnalysisHooks !== []);
                $flags |= (int) ($plugin->afterFileAnalysisHooks !== []) << 1;
                $flags |= (int) ($plugin->afterAnalysisHooks !== []) << 2;
                $flags |= (int) ($plugin->initializationHooks !== []) << 3;
                foreach ($plugin->afterFileAnalysisHooks as $hook) {
                    foreach ($hook->getRequirements() as $requirement) {
                        $flags |= (int) ($requirement === FileAnalysisRequirement::ExpressionTypes) << 4;
                    }
                }
                $writer->writeU8($flags);
                $writer->writeCount($definition->aliases);
                foreach ($definition->aliases as $alias) {
                    $writer->writeString($alias);
                }

                self::writeTargetRegistrations($writer, $plugin->functionProviders, self::REGISTRATION_CAPABILITIES);
                self::writeTargetRegistrations($writer, $plugin->methodProviders, self::REGISTRATION_CAPABILITIES);
                self::writeTargetRegistrations($writer, $plugin->propertyProviders);
                self::writeTargetRegistrations($writer, $plugin->propertyInitializationProviders);
                self::writeTargetRegistrations($writer, $plugin->classInitializerProviders);

                $writer->writeCount($plugin->entryPoints);
                foreach ($plugin->entryPoints as $entryPoint) {
                    $writer->writeBytes($entryPoint->class);
                    $writer->writeBytes($entryPoint->method);
                }

                $writer->writeCount($plugin->attributedEntryPoints);
                foreach ($plugin->attributedEntryPoints as $entryPoint) {
                    $writer->writeBytes($entryPoint->class->class);
                    $writer->writeBytes($entryPoint->attribute);
                }

                $writer->writeCount($plugin->issueFilterHooks);
                foreach ($plugin->issueFilterHooks as $hook) {
                    $writer->writeU16($hook->index);
                    $writer->writeCount($hook->targets);
                    foreach ($hook->targets as $code) {
                        $writer->writeString($code);
                    }
                }

                self::writeTargetRegistrations($writer, $plugin->nodeAnalysisHooks);
                self::writeTargetRegistrations($writer, $plugin->methodCallAnalysisHooks);
                self::writeTargetRegistrations($writer, $plugin->classLikeAnalysisHooks);
                self::writeTargetRegistrations($writer, $plugin->functionAssertionProviders);
                self::writeTargetRegistrations($writer, $plugin->methodAssertionProviders);
                self::writeTargetRegistrations($writer, $plugin->codebaseScanHooks);
                self::writeTargetRegistrations($writer, $plugin->callForwardingProviders);
            }
        }

        return $writer->finish();
    }

    /**
     * @param list<RegisteredTargetedCallback> $registrations
     */
    private static function writeTargetRegistrations(ValueWriter $writer, array $registrations, int $header = 0): void
    {
        $writer->writeCount($registrations);
        foreach ($registrations as $registration) {
            $writer->writeU16($registration->index);
            self::writeRegistrationHeader($writer, $registration, $header);

            /** @var non-empty-list<FunctionTarget|MethodTarget|PropertyTarget|ClassTarget|NodeKind|ClassLikeTarget|non-empty-string> $targets */
            $targets = $registration->targets;
            $writer->writeCount($targets);
            foreach ($targets as $target) {
                self::writeRegistrationTarget($writer, $target);
            }
        }
    }

    private static function writeRegistrationHeader(
        ValueWriter $writer,
        RegisteredTargetedCallback $registration,
        int $header,
    ): void {
        if ($registration->callback instanceof TargetedAnalysisHook) {
            $writer->writeU8(self::requirements($registration->requirements));

            return;
        }

        if ($header === self::REGISTRATION_CAPABILITIES) {
            $writer->writeU8(
                (int) ($registration->callback instanceof CallableSignatureProvider)
                | ((int) ($registration->callback instanceof CallableSignatureOverride) << 1)
                | ((int) ($registration->callback instanceof UndeclaredReturnTypeProvider) << 2),
            );
        }
    }

    /** @param list<FileAnalysisRequirement> $requirements */
    private static function requirements(array $requirements): int
    {
        $flags = 0;
        foreach ($requirements as $requirement) {
            $flags |= 1
            << match ($requirement) {
                FileAnalysisRequirement::ExpressionTypes => 0,
                FileAnalysisRequirement::TargetExpressionTypes => 1,
                FileAnalysisRequirement::ReceiverType => 2,
                FileAnalysisRequirement::ArgumentTypes => 3,
                FileAnalysisRequirement::TargetSubtree => 4,
                FileAnalysisRequirement::SourceText => 5,
                FileAnalysisRequirement::VariableDefinedness => 6,
            };
        }

        return $flags;
    }

    private static function writeRegistrationTarget(
        ValueWriter $writer,
        FunctionTarget|MethodTarget|PropertyTarget|ClassTarget|NodeKind|ClassLikeTarget|string $target,
    ): void {
        if ($target instanceof FunctionTarget) {
            $writer->writeU8($target->kind->value);
            $writer->writeBytes($target->value);

            return;
        }

        if ($target instanceof MethodTarget) {
            $writer->writeBytes($target->class);
            $writer->writeBytes($target->method);

            return;
        }

        if ($target instanceof PropertyTarget) {
            $writer->writeBytes($target->class);
            $writer->writeBytes($target->property);

            return;
        }

        if (is_string($target)) {
            $writer->writeString($target);

            return;
        }

        $writer->writeBytes(match (true) {
            $target instanceof ClassTarget => $target->class,
            $target instanceof NodeKind => $target->value,
            $target instanceof ClassLikeTarget => $target->ancestor,
        });
    }

    /**
     * @return array{bool, bool, list<int<0, 65535>>, array<int<0, 65535>, list<CodebaseScanFile>>}
     */
    public static function readCodebaseScanRequest(ValueReader $reader): array
    {
        $firstBatch = $reader->readBoolean();
        $lastBatch = $reader->readBoolean();
        $activeHookCount = $reader->readCount(65_536);
        if ($activeHookCount === 0) {
            throw new ProtocolException('A codebase-scan request has no active hooks.');
        }
        $activeHooks = [];
        for ($hookIndex = 0; $hookIndex < $activeHookCount; ++$hookIndex) {
            $activeHooks[] = $reader->readU16();
        }

        $fileCount = $reader->readCount(1_000_000);
        $filesByHook = [];
        for ($fileIndex = 0; $fileIndex < $fileCount; ++$fileIndex) {
            $hookCount = $reader->readCount(65_536);
            if ($hookCount === 0) {
                throw new ProtocolException('A codebase-scan file has no matching hooks.');
            }

            $hooks = [];
            for ($hookIndex = 0; $hookIndex < $hookCount; ++$hookIndex) {
                $hooks[] = $reader->readU16();
            }

            $path = $reader->readBytes();
            if ($path === '') {
                throw new ProtocolException('A codebase-scan file has an empty path.');
            }
            $file = self::readCodebaseScanFile($reader, $path);
            foreach ($hooks as $hook) {
                $filesByHook[$hook][] = $file;
            }
        }
        $reader->finish();

        return [$firstBatch, $lastBatch, $activeHooks, $filesByHook];
    }

    private static function readCodebaseScanFile(ValueReader $reader, string $path): CodebaseScanFile
    {
        $classLikes = [];
        $properties = [];
        $classLikeCount = $reader->readCount(1_000_000);
        for ($classLikeIndex = 0; $classLikeIndex < $classLikeCount; ++$classLikeIndex) {
            $classLike = MetadataCodec::readClassLike($reader);
            $classLikes[] = $classLike;
            $propertyCount = $reader->readCount(65_536);
            for ($propertyIndex = 0; $propertyIndex < $propertyCount; ++$propertyIndex) {
                $properties[$classLike->name][] = MetadataCodec::readProperty($reader);
            }
        }

        $functionLikes = [];
        $functionLikeCount = $reader->readCount(1_000_000);
        for ($functionLikeIndex = 0; $functionLikeIndex < $functionLikeCount; ++$functionLikeIndex) {
            $functionLikes[] = MetadataCodec::readFunctionLike($reader);
        }

        return new CodebaseScanFile($path, $classLikes, $properties, $functionLikes);
    }

    /**
     * @param list<array{int<0, 65535>, ClassLikeRefinement|FunctionLikeRefinement}> $refinements each paired with the hook that described it
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeCodebaseScanResponse(array $refinements): array
    {
        $writer = self::createMessage(self::CODEBASE_SCAN_RESPONSE);
        $writer->writeCount($refinements);
        foreach ($refinements as [$hook, $refinement]) {
            $writer->writeU16($hook);
            $writer->writeBytes($refinement->file);
            self::writeDeclaration($writer, $refinement);
            $writer->writeCount($refinement->issues);
            foreach ($refinement->issues as $issue) {
                $writer->writeU8($issue->level->value);
                $writer->writeString($issue->code);
                $writer->writeString($issue->message);
                $writer->writeU32($issue->span->start);
                $writer->writeU32($issue->span->end);
            }
        }

        return $writer->finish();
    }

    private static function writeDeclaration(
        ValueWriter $writer,
        ClassLikeRefinement|FunctionLikeRefinement $refinement,
    ): void {
        if ($refinement instanceof FunctionLikeRefinement) {
            $writer->writeU8(2);
            self::writeDeclarationName($writer, $refinement->declaredAt, $refinement->function);
            self::writeSignature($writer, $refinement->signature);

            return;
        }

        $writer->writeU8(1);
        self::writeDeclarationName($writer, $refinement->declaredAt, $refinement->class);
        self::writeTypeParameters($writer, $refinement->typeParameters);
        $writer->writeCount($refinement->inherited);
        foreach ($refinement->inherited as $inherited) {
            $writer->writeBytes($inherited->ancestor);
            $writer->writeU32($inherited->span->start);
            $writer->writeU32($inherited->span->end);
            $writer->writeCount($inherited->arguments);
            foreach ($inherited->arguments as $argument) {
                self::writeDeclaredType($writer, $argument);
            }
        }

        $writer->writeCount($refinement->requiredExtends);
        foreach ($refinement->requiredExtends as $required) {
            $writer->writeBytes($required);
        }

        self::writeRefinedMembers($writer, $refinement->properties);
        $writer->writeCount($refinement->methods);
        foreach ($refinement->methods as $method => $signature) {
            $writer->writeBytes($method);
            self::writeSignature($writer, $signature);
        }
    }

    /**
     * An anonymous declaration is named by the offset it starts at.
     *
     * @param int<0, max>|null $declaredAt
     */
    private static function writeDeclarationName(ValueWriter $writer, ?int $declaredAt, string $name): void
    {
        $writer->writeBoolean($declaredAt !== null);
        if ($declaredAt !== null) {
            $writer->writeU32($declaredAt);

            return;
        }

        $writer->writeBytes($name);
    }

    private static function writeSignature(ValueWriter $writer, SignatureRefinement $signature): void
    {
        self::writeTypeParameters($writer, $signature->typeParameters);
        self::writeRefinedMembers($writer, $signature->parameters);
        self::writeRefinedMembers($writer, $signature->closureThis);
        $writer->writeBoolean($signature->return !== null);
        if ($signature->return !== null) {
            self::writeRefinedType($writer, $signature->return);
        }

        $writer->writeBoolean($signature->receiver !== null);
        if ($signature->receiver !== null) {
            self::writeRefinedType($writer, $signature->receiver);
        }

        $writer->writeBoolean($signature->returnFromBody);
    }

    /** @param array<non-empty-string, RefinedType> $members */
    private static function writeRefinedMembers(ValueWriter $writer, array $members): void
    {
        $writer->writeCount($members);
        foreach ($members as $name => $type) {
            $writer->writeBytes($name);
            self::writeRefinedType($writer, $type);
        }
    }

    /** @param list<TypeParameter> $typeParameters */
    private static function writeTypeParameters(ValueWriter $writer, array $typeParameters): void
    {
        $writer->writeCount($typeParameters);
        foreach ($typeParameters as $typeParameter) {
            $writer->writeBytes($typeParameter->name);
            self::writeRefinedType($writer, $typeParameter->bound);
            $writer->writeU8(match ($typeParameter->variance) {
                Variance::Invariant => 1,
                Variance::Covariant => 2,
                Variance::Contravariant => 3,
                Variance::Bivariant => throw new ProtocolException('A type parameter is never bivariant.'),
            });
        }
    }

    private static function writeRefinedType(ValueWriter $writer, RefinedType $type): void
    {
        self::writeDeclaredType($writer, $type->type);
        $writer->writeU32($type->span->start);
        $writer->writeU32($type->span->end);
    }

    private static function writeDeclaredType(ValueWriter $writer, Type $type): void
    {
        if ($type->isRequestReference()) {
            throw new ProtocolException(
                'A declaration refinement cannot use a type borrowed from an analyzer request.',
            );
        }

        $writer->writeValues($type->encode());
    }

    /**
     * @param array<string, Span> $spans
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeAnalysisTypeQuery(
        int $generation,
        string $file,
        int $operation,
        array $spans = [],
    ): array {
        $writer = self::createMessage(self::ANALYSIS_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8($operation);
        $writer->writeBytes($file);
        if ($operation === self::GET_EXPRESSION_TYPES) {
            $writer->writeCount($spans);
            foreach ($spans as $span) {
                $writer->writeU32($span->start);
                $writer->writeU32($span->end);
            }
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{list<Type|null>, list<int|Type>}
     */
    public static function readOptionalAnalysisTypeQueryResponse(
        array $payload,
        int $generation,
        string $file,
        int $operation,
    ): array {
        $reader = self::readAnalysisTypeQueryResponsePayload($payload, $generation, $file, $operation);
        $count = $reader->readCount(1_000_000);
        $types = [];
        for ($index = 0; $index < $count; ++$index) {
            if (!$reader->readBoolean()) {
                $types[] = null;
                continue;
            }

            $types[] = TypeCodec::readComplete($reader);
        }

        $prefetched = [];
        $count = $reader->readCount(1_000_000);
        for ($index = 0; $index < $count; ++$index) {
            $prefetched[] = $reader->readU32();
            $prefetched[] = $reader->readU32();
            $prefetched[] = TypeCodec::readComplete($reader);
        }

        $reader->finish();

        return [$types, $prefetched];
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<Type>
     */
    public static function readAnalysisTypeQueryResponse(
        array $payload,
        int $generation,
        string $file,
        int $operation,
    ): array {
        $reader = self::readAnalysisTypeQueryResponsePayload($payload, $generation, $file, $operation);
        $count = $reader->readCount(1_000_000);
        $types = [];
        for ($index = 0; $index < $count; ++$index) {
            $types[] = TypeCodec::readComplete($reader);
        }

        $reader->finish();

        return $types;
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<ExpressionType>
     */
    public static function readAllExpressionTypesResponse(array $payload, int $generation, string $file): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if (
            $kind !== self::ANALYSIS_QUERY_RESPONSE
            || $reader->readU64() !== $generation
            || $reader->readU8() !== self::GET_ALL_EXPRESSION_TYPES
            || $reader->readBytes() !== $file
        ) {
            throw new ProtocolException('An expression-type response does not match its request.');
        }

        $count = $reader->readCount(1_000_000);
        $types = [];
        for ($index = 0; $index < $count; ++$index) {
            $types[] = new ExpressionType(
                new Span($reader->readU32(), $reader->readU32()),
                TypeCodec::readComplete($reader),
            );
        }
        $reader->finish();

        return $types;
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     * @param list<NodeKind> $kinds
     */
    public static function readSourceFileResponse(
        array $payload,
        int $generation,
        string $file,
        PHPVersion $phpVersion,
        array $kinds,
    ): SourceFile {
        $reader = self::readAnalysisTypeQueryResponsePayload($payload, $generation, $file, self::GET_SOURCE_FILE);
        $source = SourceFileCodec::readValues($reader, $phpVersion, $kinds, $file, $reader->readBytes());
        $reader->finish();

        return $source;
    }

    /**
     * @param list<NodeKind> $nodeKinds
     */
    public static function readLifecycleRequest(
        int $kind,
        ValueReader $reader,
        HostClient $host,
        PHPVersion $phpVersion,
        array $nodeKinds,
    ): LifecycleRequest {
        $generation = $reader->readU64();
        $pluginCount = $reader->readU16();
        if ($pluginCount === 0) {
            throw new ProtocolException('An analyzer lifecycle request contains no plugins.');
        }
        $plugins = [];
        for ($index = 0; $index < $pluginCount; ++$index) {
            $plugins[] = $reader->readU16();
        }

        $analysis = match ($kind) {
            self::BEFORE_ANALYSIS_REQUEST => null,
            self::AFTER_FILE_ANALYSIS_REQUEST => self::readFileAnalysis(
                $reader,
                $host,
                $generation,
                $phpVersion,
                $nodeKinds,
            ),
            self::AFTER_FILE_ANALYSIS_BATCH_REQUEST => self::readFileAnalysisBatch(
                $reader,
                $host,
                $generation,
                $phpVersion,
                $nodeKinds,
            ),
            self::AFTER_ANALYSIS_REQUEST => self::readProjectAnalysis(
                $reader,
                $host,
                $generation,
                $phpVersion,
                $nodeKinds,
            ),
            default => throw new ProtocolException("Unknown analyzer lifecycle request kind {$kind}."),
        };

        $reader->finish();

        return new LifecycleRequest($generation, $plugins, $analysis);
    }

    /** @param list<null|bool|int|float|string|list<mixed>> $payload */
    private static function readAnalysisTypeQueryResponsePayload(
        array $payload,
        int $generation,
        string $file,
        int $operation,
    ): ValueReader {
        [$kind, $reader] = self::readRequest($payload);
        if (
            $kind !== self::ANALYSIS_QUERY_RESPONSE
            || $reader->readU64() !== $generation
            || $reader->readU8() !== $operation
            || $reader->readBytes() !== $file
        ) {
            throw new ProtocolException('An analysis artifact response does not match its request.');
        }

        return $reader;
    }

    /**
     * @param list<NodeKind> $nodeKinds
     */
    private static function readProjectAnalysis(
        ValueReader $reader,
        HostClient $host,
        int $generation,
        PHPVersion $phpVersion,
        array $nodeKinds,
    ): ProjectAnalysis {
        $issueCount = $reader->readU32();
        $summary = self::readReferenceSummary($reader);
        $fileCount = $reader->readCount(1_000_000);
        $files = [];
        for ($index = 0; $index < $fileCount; ++$index) {
            $files[] = self::readFileAnalysis($reader, $host, $generation, $phpVersion, $nodeKinds);
        }

        return new ProjectAnalysis($files, $issueCount, new SymbolReferences($host, $generation, $summary));
    }

    /**
     * @param list<int|ReportedIssue|string|null> $reportedIssues each issue as its plugin, the issue, and the file
     *   whose after-file hooks reported it, which a project-level hook's issue has none of. Mago keeps an after-file
     *   issue with that file's analysis, so it stays until the hooks of that file run again.
     * @param list<int|string|MemberIdentifier|ReferenceOrigin|ReferenceKind|null> $contributedReferences
     * @param list<array{string|null, CodebaseReads, CodebaseReads, int}> $recordedReads each file's codebase reads,
     *   or the before-analysis hooks', which name no file: the reads of plugins with no provider, then those of plugins
     *   with one, then the end of `$classLoads` when the hooks finished, each list written through `$sentReads` for
     *   `$generation`
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeLifecycleResponse(
        int $requestKind,
        ClassLoads $classLoads,
        SentReads $sentReads,
        int $generation,
        array $reportedIssues,
        array $contributedReferences = [],
        array $recordedReads = [],
    ): array {
        $responseKind = match ($requestKind) {
            self::BEFORE_ANALYSIS_REQUEST => self::BEFORE_ANALYSIS_RESPONSE,
            self::AFTER_FILE_ANALYSIS_REQUEST => self::AFTER_FILE_ANALYSIS_RESPONSE,
            self::AFTER_ANALYSIS_REQUEST => self::AFTER_ANALYSIS_RESPONSE,
            self::AFTER_FILE_ANALYSIS_BATCH_REQUEST => self::AFTER_FILE_ANALYSIS_BATCH_RESPONSE,
            default => throw new ProtocolException("Unknown analyzer lifecycle request kind {$requestKind}."),
        };

        $writer = self::createMessage($responseKind);
        $writer->writeU32(intdiv(count($reportedIssues), 3));
        for ($index = 0, $count = count($reportedIssues); $index < $count; $index += 3) {
            /** @var int<0, 65535> $plugin */
            $plugin = $reportedIssues[$index];
            /** @var ReportedIssue $reported */
            $reported = $reportedIssues[$index + 1];
            /** @var string|null $reportingFile */
            $reportingFile = $reportedIssues[$index + 2];
            $issue = $reported->issue;
            $writer->writeU16($plugin);
            if (
                $requestKind === self::AFTER_FILE_ANALYSIS_REQUEST
                || $requestKind === self::AFTER_FILE_ANALYSIS_BATCH_REQUEST
            ) {
                $writer->writeBytes(
                    $reportingFile ?? throw new ProtocolException(
                        'An after-file issue names no file that reported it.',
                    ),
                );
            }

            $writer->writeU8($reported->level->value);
            $writer->writeString($reported->code);
            $writer->writeString($issue->message);
            $writer->writeCount($issue->notes);
            foreach ($issue->notes as $note) {
                $writer->writeString($note);
            }

            $writer->writeOptionalString($issue->help);
            $writer->writeOptionalString($issue->link);
            $writer->writeCount($issue->annotations);
            foreach ($issue->annotations as $annotation) {
                $writer->writeU8($annotation->kind->value);
                $writer->writeOptionalString($annotation->file ?? $reportingFile);
                $writer->writeU32($annotation->span->start);
                $writer->writeU32($annotation->span->end);
                $writer->writeOptionalString($annotation->message);
            }

            $writer->writeCount($issue->edits);
            foreach ($issue->edits as $edit) {
                $writer->writeOptionalString($edit->file ?? $reportingFile);
                $writer->writeU32($edit->span->start);
                $writer->writeU32($edit->span->end);
                $writer->writeU8($edit->safety->value);
                $writer->writeBytes($edit->newText);
            }
        }

        $writer->writeU32(intdiv(count($contributedReferences), 5));
        for ($index = 0, $count = count($contributedReferences); $index < $count; $index += 5) {
            /** @var int<0, 65535> $plugin */
            $plugin = $contributedReferences[$index];
            /** @var string|MemberIdentifier|ReferenceOrigin $source */
            $source = $contributedReferences[$index + 1];
            /** @var string|MemberIdentifier $target */
            $target = $contributedReferences[$index + 2];
            /** @var ReferenceKind $kind */
            $kind = $contributedReferences[$index + 3];
            /** @var string|null $discoveryFile */
            $discoveryFile = $contributedReferences[$index + 4];
            $writer->writeU16($plugin);
            if ($requestKind !== self::BEFORE_ANALYSIS_REQUEST) {
                $writer->writeString(
                    $discoveryFile ?? throw new ProtocolException(
                        'A late symbol-reference contribution has no discovery file.',
                    ),
                );
            }

            self::writeReferenceOrigin(
                $writer,
                $source instanceof ReferenceOrigin ? $source : ReferenceOrigin::symbol($source),
            );
            self::writeSymbolIdentifier($writer, $target);
            $writer->writeU8($kind->value);
        }

        $writer->writeCount($recordedReads);
        foreach ($recordedReads as [$file, $issueReads, $codeReads, $end]) {
            if ($requestKind !== self::BEFORE_ANALYSIS_REQUEST) {
                $writer->writeBytes($file ?? throw new ProtocolException('A file analysis read names no file.'));
            }

            self::writeCodebaseReads($writer, $sentReads, $generation, $issueReads, $codeReads, $classLoads, $end);
        }

        return $writer->finish();
    }

    /**
     * Places the codebase reads a provider or issue filter made right after `$response`'s header, with the class loads
     * its worker made up to now, so Mago reads them in one place whatever kind the response is.
     *
     * @param list<null|bool|int|float|string|list<mixed>> $response
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function withCodebaseReads(
        array $response,
        SentReads $sentReads,
        int $generation,
        CodebaseReads $issueReads,
        CodebaseReads $codeReads,
        ClassLoads $classLoads,
    ): array {
        $writer = new ValueWriter(array_slice($response, 0, 2));
        self::writeCodebaseReads(
            $writer,
            $sentReads,
            $generation,
            $issueReads,
            $codeReads,
            $classLoads,
            $classLoads->end(),
        );
        $writer->writeValues(array_slice($response, 2));

        return $writer->finish();
    }

    /**
     * Writes the reads of plugins with no provider, which decide only the issues reported, then those of plugins with
     * one, which can change the analyzed code, each list through `$sentReads` for `$generation`, then the class loads
     * of the worker up to `$end`, which the callbacks read.
     */
    private static function writeCodebaseReads(
        ValueWriter $writer,
        SentReads $sentReads,
        int $generation,
        CodebaseReads $issueReads,
        CodebaseReads $codeReads,
        ClassLoads $classLoads,
        int $end,
    ): void {
        $sentReads->write($writer, $generation, $issueReads);
        $sentReads->write($writer, $generation, $codeReads);

        $classLoads->write($writer, $end);
    }

    /**
     * @param list<NodeKind> $nodeKinds
     */
    private static function readFileAnalysis(
        ValueReader $reader,
        HostClient $host,
        int $generation,
        PHPVersion $phpVersion,
        array $nodeKinds,
    ): FileAnalysis {
        $file = $reader->readBytes();
        if ($file === '') {
            throw new ProtocolException('An analyzer file result contains an empty file name.');
        }

        $size = $reader->readU32();
        $expressionCount = $reader->readU32();
        $inferredReturnCount = $reader->readU32();
        $inferredYieldKeyCount = $reader->readU32();
        $inferredYieldValueCount = $reader->readU32();
        $references = self::readReferenceSummary($reader);
        $hasLocalExpressionTypes = $reader->readBoolean();
        $expressionTypeRecords = '';
        $encodedExpressionTypes = [];
        if ($hasLocalExpressionTypes) {
            $localExpressionCount = $reader->readCount(1_000_000);
            $expressionTypeRecords = $reader->readBytes();
            if (
                $localExpressionCount !== $expressionCount
                || strlen($expressionTypeRecords) !== ($expressionCount * 16)
            ) {
                throw new ProtocolException('An eager expression-type index does not match its file summary.');
            }
            $encodedExpressionTypes = $reader->readList();
        }
        $sourceFile = null;
        $nodeAnalysisData = [];
        if ($reader->readBoolean()) {
            $backend = $reader->readU16();
            $contents = $reader->readBoolean() ? $reader->readBytes() : '';
            $sourceFile = SourceFileCodec::readValues($reader, $phpVersion, $nodeKinds, $file, $contents);
            $targetCount = $reader->readCount(1_000_000);
            if ($targetCount !== count($sourceFile->getTargetNodes())) {
                throw new ProtocolException('Node-analysis data does not match the targeted syntax snapshot.');
            }
            for ($index = 0; $index < $targetCount; ++$index) {
                $requirements = $reader->readU8();
                $targetType = ($requirements & (1 << 1)) !== 0 ? self::readOptionalType($reader) : null;
                $receiverType = ($requirements & (1 << 2)) !== 0 ? self::readOptionalType($reader) : null;
                $argumentTypes = [];
                if (($requirements & (1 << 3)) !== 0) {
                    $argumentCount = $reader->readCount(1_000_000);
                    for ($argumentIndex = 0; $argumentIndex < $argumentCount; ++$argumentIndex) {
                        $argumentTypes[] = self::readOptionalType($reader);
                    }
                }
                $variableDefinedness = null;
                if (($requirements & (1 << 6)) !== 0 && $reader->readBoolean()) {
                    $variableDefinedness = [];
                    $variableCount = $reader->readCount(1_000_000);
                    for ($variableIndex = 0; $variableIndex < $variableCount; ++$variableIndex) {
                        $variable = $reader->readBytes();
                        if ($variable === '' || $variable[0] !== '$') {
                            throw new ProtocolException(
                                'A variable-definedness record contains an invalid variable name.',
                            );
                        }
                        if (array_key_exists($variable, $variableDefinedness)) {
                            throw new ProtocolException("A variable-definedness snapshot repeats `{$variable}`.");
                        }

                        $variableDefinedness[$variable] = match ($reader->readU8()) {
                            1 => VariableDefinedness::Defined,
                            2 => VariableDefinedness::PossiblyDefined,
                            default => throw new ProtocolException(
                                'A variable-definedness record has an unknown state.',
                            ),
                        };
                    }
                }
                $targetedHookIndices = [];
                $routeCount = $reader->readCount(1_000_000);
                for ($routeIndex = 0; $routeIndex < $routeCount; ++$routeIndex) {
                    $route = $reader->readU32();
                    if (($route >> 16) === $backend) {
                        $targetedHookIndices[] = $route & 0xffff;
                    }
                }
                $nodeAnalysisData[] = new NodeAnalysisData(
                    $targetType,
                    $receiverType,
                    $argumentTypes,
                    $variableDefinedness,
                    $targetedHookIndices,
                    $reader->readBoolean()
                        ? $nodeKinds[$reader->readU16()] ?? throw new ProtocolException(
                            'A node-analysis target means an unknown node kind.',
                        )
                        : null,
                );
            }
        }

        return new FileAnalysis(
            $host,
            $generation,
            $phpVersion,
            $nodeKinds,
            $sourceFile,
            $nodeAnalysisData,
            $hasLocalExpressionTypes,
            $expressionTypeRecords,
            $encodedExpressionTypes,
            $file,
            $size,
            $expressionCount,
            $inferredReturnCount,
            $inferredYieldKeyCount,
            $inferredYieldValueCount,
            $references,
        );
    }

    /**
     * @param list<NodeKind> $nodeKinds
     *
     * @return non-empty-list<FileAnalysis>
     */
    private static function readFileAnalysisBatch(
        ValueReader $reader,
        HostClient $host,
        int $generation,
        PHPVersion $phpVersion,
        array $nodeKinds,
    ): array {
        $count = $reader->readCount(1_000_000);
        if ($count === 0) {
            throw new ProtocolException('An analyzer after-file batch contains no files.');
        }

        $files = [];
        for ($index = 0; $index < $count; ++$index) {
            $files[] = self::readFileAnalysis($reader, $host, $generation, $phpVersion, $nodeKinds);
        }

        return $files;
    }

    private static function readReferenceSummary(ValueReader $reader): ReferenceSummary
    {
        return new ReferenceSummary($reader->readU64(), $reader->readU64(), $reader->readU64());
    }

    private static function readOptionalType(ValueReader $reader): ?Type
    {
        return $reader->readBoolean() ? TypeCodec::readComplete($reader) : null;
    }

    /** Reads a return-type request, which mago's engine builds whole. */
    public static function readReturnTypeRequest(ValueReader $reader): ReturnTypeRequest
    {
        $request = $reader->readObject(ReturnTypeRequest::class);
        $reader->finish();

        return $request;
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writeReturnTypeResponse(?Type $type): array
    {
        $writer = self::createMessage(self::RETURN_TYPE_RESPONSE);
        $writer->writeBoolean($type !== null);
        if ($type !== null) {
            $writer->writeValues($type->encode());
        }

        return $writer->finish();
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writeCallableSignatureResponse(?EffectiveCallableSignature $signature): array
    {
        $writer = self::createMessage(self::CALLABLE_SIGNATURE_RESPONSE);
        if ($signature === null) {
            $writer->writeBoolean(false);

            return $writer->finish();
        }

        $writer->writeBoolean(true);
        $writer->writeOptionalString($signature->displayName);
        $writer->writeBoolean($signature->allowsNamedArguments);
        $writer->writeCount($signature->parameters);
        foreach ($signature->parameters as $parameter) {
            $writer->writeOptionalString($parameter->name);
            $writer->writeBoolean($parameter->type !== null);
            if ($parameter->type !== null) {
                $writer->writeValues($parameter->type->encode());
            }

            $writer->writeBoolean($parameter->closureThisType !== null);
            if ($parameter->closureThisType !== null) {
                $writer->writeValues($parameter->closureThisType->encode());
            }

            $flags = 0;
            $flags |= (int) $parameter->byReference;
            $flags |= (int) $parameter->variadic << 1;
            $flags |= (int) $parameter->hasDefault << 2;
            $writer->writeU8($flags);
        }

        return $writer->finish();
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writeAssertionResponse(?InvocationAssertions $assertions): array
    {
        $writer = self::createMessage(self::ASSERTION_RESPONSE);
        if ($assertions === null || $assertions->isEmpty()) {
            $writer->writeBoolean(false);

            return $writer->finish();
        }

        $writer->writeBoolean(true);
        AssertionCodec::write($writer, $assertions);

        return $writer->finish();
    }

    /** Reads a property-type request, which mago's engine builds whole. */
    public static function readPropertyTypeRequest(ValueReader $reader): PropertyTypeRequest
    {
        $request = $reader->readObject(PropertyTypeRequest::class);
        $reader->finish();

        return $request;
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writePropertyTypeResponse(?PropertyType $type): array
    {
        $writer = self::createMessage(self::PROPERTY_TYPE_RESPONSE);
        if ($type === null) {
            $writer->writeBoolean(false);

            return $writer->finish();
        }

        $writer->writeBoolean(true);
        $writer->writeBoolean($type->readType !== null);
        if ($type->readType !== null) {
            $writer->writeValues($type->readType->encode());
        }

        $writer->writeBoolean($type->writeType !== null);
        if ($type->writeType !== null) {
            $writer->writeValues($type->writeType->encode());
        }

        return $writer->finish();
    }

    /** Reads a call-forwarding request, which mago's engine builds whole. */
    public static function readCallForwardingRequest(ValueReader $reader): CallForwardingRequest
    {
        $request = $reader->readObject(CallForwardingRequest::class);
        $reader->finish();

        return $request;
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writeCallForwardingResponse(?ForwardedCall $forwarded): array
    {
        $writer = self::createMessage(self::CALL_FORWARDING_RESPONSE);
        $writer->writeBoolean($forwarded !== null);
        if ($forwarded !== null) {
            $writer->writeValues($forwarded->receiver->encode());
            $writer->writeCount($forwarded->methods);
            foreach ($forwarded->methods as $method) {
                $writer->writeBytes($method);
            }
        }

        return $writer->finish();
    }

    public static function readPropertyInitializationRequest(ValueReader $reader): PropertyInitializationRequest
    {
        $generation = $reader->readU64();
        $providerCount = $reader->readU16();
        if ($providerCount === 0) {
            throw new ProtocolException('A property-initialization request contains no providers.');
        }

        $providers = [];
        for ($index = 0; $index < $providerCount; ++$index) {
            $providers[] = $reader->readU16();
        }

        $declaringClass = $reader->readBytes();
        if ($declaringClass === '') {
            throw new ProtocolException('A property-initialization request has an empty declaring class.');
        }

        $property = MetadataCodec::readProperty($reader);
        $reader->finish();

        return new PropertyInitializationRequest($generation, $providers, $declaringClass, $property);
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writePropertyInitializationResponse(bool $initialized): array
    {
        $writer = self::createMessage(self::PROPERTY_INITIALIZATION_RESPONSE);
        $writer->writeBoolean($initialized);

        return $writer->finish();
    }

    public static function readClassInitializerRequest(ValueReader $reader): ClassInitializerRequest
    {
        $generation = $reader->readU64();
        $providerCount = $reader->readU16();
        if ($providerCount === 0) {
            throw new ProtocolException('A class-initializer request contains no providers.');
        }

        $providers = [];
        for ($index = 0; $index < $providerCount; ++$index) {
            $providers[] = $reader->readU16();
        }

        $class = MetadataCodec::readClassLike($reader);
        $reader->finish();

        return new ClassInitializerRequest($generation, $providers, $class);
    }

    /**
     * @param list<non-empty-string> $initializers
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeClassInitializerResponse(array $initializers): array
    {
        $writer = self::createMessage(self::CLASS_INITIALIZER_RESPONSE);
        $writer->writeCount($initializers);
        foreach ($initializers as $initializer) {
            $writer->writeBytes($initializer);
        }

        return $writer->finish();
    }

    public static function readIssueFilterRequest(ValueReader $reader): IssueFilterRequest
    {
        $generation = $reader->readU64();
        $hookCount = $reader->readU16();
        if ($hookCount === 0) {
            throw new ProtocolException('An issue-filter request contains no hooks.');
        }

        $hooks = [];
        for ($index = 0; $index < $hookCount; ++$index) {
            $hooks[] = $reader->readU16();
        }

        $file = $reader->readBytes();
        if ($file === '') {
            throw new ProtocolException('An issue-filter request has an empty file name.');
        }

        $contents = $reader->readBytes();
        $issueCount = $reader->readCount(self::MAXIMUM_ISSUES);
        if ($issueCount === 0) {
            throw new ProtocolException('An issue-filter request contains no issues.');
        }

        $issues = [];
        for ($index = 0; $index < $issueCount; ++$index) {
            $issues[] = self::readReportedIssue($reader);
        }

        $reader->finish();
        return new IssueFilterRequest($generation, $hooks, $file, $contents, $issues);
    }

    /**
     * @param list<int<0, max>> $removedIndices
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeIssueFilterResponse(int $issueCount, array $removedIndices): array
    {
        $writer = self::createMessage(self::ISSUE_FILTER_RESPONSE);
        $writer->writeU32($issueCount);
        $writer->writeCount($removedIndices);
        foreach ($removedIndices as $index) {
            $writer->writeU32($index);
        }

        return $writer->finish();
    }

    private static function readReportedIssue(ValueReader $reader): ReportingReportedIssue
    {
        $level = match ($value = $reader->readU8()) {
            1 => Level::Note,
            2 => Level::Help,
            3 => Level::Warning,
            4 => Level::Error,
            default => throw new ProtocolException("Invalid issue-filter severity {$value}."),
        };

        $code = $reader->readOptionalString();
        if ($code === '') {
            throw new ProtocolException('An issue-filter candidate has an empty issue code.');
        }

        $message = $reader->readString();
        if ($message === '') {
            throw new ProtocolException('An issue-filter candidate has an empty message.');
        }

        $noteCount = $reader->readCount(self::MAXIMUM_ISSUE_NOTES);
        $notes = [];
        for ($index = 0; $index < $noteCount; ++$index) {
            $note = $reader->readString();
            if ($note === '') {
                throw new ProtocolException('An issue-filter candidate has an empty note.');
            }

            $notes[] = $note;
        }

        $help = $reader->readOptionalString();
        $link = $reader->readOptionalString();
        $annotationCount = $reader->readCount(self::MAXIMUM_ISSUE_ANNOTATIONS);
        $annotations = [];
        for ($index = 0; $index < $annotationCount; ++$index) {
            $kind = match ($value = $reader->readU8()) {
                1 => AnnotationKind::Primary,
                2 => AnnotationKind::Secondary,
                default => throw new ProtocolException("Invalid issue-filter annotation kind {$value}."),
            };

            $file = $reader->readOptionalString();
            $span = new Span($reader->readU32(), $reader->readU32());
            $annotations[] = new Annotation($kind, $span, $reader->readOptionalString(), $file);
        }

        $editCount = $reader->readCount(self::MAXIMUM_ISSUE_EDITS);
        $edits = [];
        for ($index = 0; $index < $editCount; ++$index) {
            $file = $reader->readOptionalString();
            $span = new Span($reader->readU32(), $reader->readU32());
            $safety = match ($value = $reader->readU8()) {
                1 => Safety::Safe,
                2 => Safety::PotentiallyUnsafe,
                3 => Safety::Unsafe,
                default => throw new ProtocolException("Invalid issue-filter edit safety {$value}."),
            };

            $edits[] = TextEdit::fromPayload($span, $reader->readBytes(), $safety, $file);
        }

        return new ReportingReportedIssue($level, $code, $message, $notes, $help, $link, $annotations, $edits);
    }

    /**
     * @param non-empty-list<null|bool|int|float|string|list<mixed>> $left
     * @param non-empty-list<null|bool|int|float|string|list<mixed>> $right
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeTypeComparisonRequest(int $operation, array $left, array $right): array
    {
        $writer = self::createMessage(self::TYPE_COMPARISON_REQUEST);
        $writer->writeU8($operation);
        $writer->writeValues($left);
        $writer->writeValues($right);

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{bool, list<string>} whether the comparison holds, and every class-like either compared type names
     */
    public static function readTypeComparisonResponse(array $payload): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::TYPE_COMPARISON_RESPONSE) {
            throw new ProtocolException("Expected a type comparison response, received analyzer message {$kind}.");
        }

        $answer = self::readTypeComparisonAnswer($reader);
        $reader->finish();

        return $answer;
    }

    /**
     * @param non-empty-list<TypeComparison> $comparisons
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeTypeComparisonBatchRequest(array $comparisons): array
    {
        $writer = self::createMessage(self::TYPE_COMPARISON_BATCH_REQUEST);
        $writer->writeCount($comparisons);
        foreach ($comparisons as $comparison) {
            $writer->writeU8($comparison->kind->value);
            $writer->writeValues($comparison->encodeLeft());
            $writer->writeValues($comparison->encodeRight());
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<array{bool, list<string>}>
     */
    public static function readTypeComparisonBatchResponse(array $payload, int $expectedCount): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::TYPE_COMPARISON_BATCH_RESPONSE) {
            throw new ProtocolException(
                "Expected a type comparison batch response, received analyzer message {$kind}.",
            );
        }

        $count = $reader->readCount(65_536);
        if ($count !== $expectedCount) {
            throw new ProtocolException("Expected {$expectedCount} type comparison results, received {$count}.");
        }

        $answers = [];
        for ($index = 0; $index < $count; ++$index) {
            $answers[] = self::readTypeComparisonAnswer($reader);
        }

        $reader->finish();
        return $answers;
    }

    /** @return array{bool, list<string>} */
    private static function readTypeComparisonAnswer(ValueReader $reader): array
    {
        $result = $reader->readBoolean();
        $classLikes = [];
        for ($index = 0, $count = $reader->readCount(65_536); $index < $count; ++$index) {
            $classLikes[] = $reader->readBytes();
        }

        return [$result, $classLikes];
    }

    /**
     * @param list<string>|list<MemberIdentifier>|list<FunctionLikeIdentifier> $keys
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeCodebaseQueryRequest(
        int $generation,
        int $operation,
        array $keys,
        ?int $classLikeKind = null,
    ): array {
        $writer = self::createMessage(self::CODEBASE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8($operation);
        if ($operation === self::GET_CLASS_LIKES) {
            $writer->writeU8($classLikeKind ?? self::ANY_CLASS_LIKE);
        }
        $writer->writeCount($keys);
        foreach ($keys as $key) {
            if ($key instanceof FunctionLikeIdentifier) {
                TypeCodec::writeFunctionLikeIdentifier($writer, $key);
                continue;
            }

            if ($key instanceof MemberIdentifier) {
                $writer->writeBytes($key->class);
                $writer->writeBytes($key->member);
                continue;
            }

            $writer->writeBytes($key);
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{ValueReader, int<0, 4294967295>}
     */
    public static function readCodebaseQueryResponse(
        array $payload,
        int $generation,
        int $operation,
        ?int $classLikeKind = null,
    ): array {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::CODEBASE_QUERY_RESPONSE) {
            throw new ProtocolException("Expected a codebase query response, received analyzer message {$kind}.");
        }
        if ($reader->readU64() !== $generation) {
            throw new ProtocolException('A codebase query response belongs to another analysis generation.');
        }
        if ($reader->readU8() !== $operation) {
            throw new ProtocolException('A codebase query response contains the wrong operation.');
        }
        if ($operation === self::GET_CLASS_LIKES && $reader->readU8() !== ($classLikeKind ?? self::ANY_CLASS_LIKE)) {
            throw new ProtocolException('A codebase query response contains the wrong class-like kind.');
        }

        return [$reader, $reader->readCount(65_536)];
    }

    /**
     * A method search as Mago reads it, both in the request that runs it and in the read that records it: the classes
     * it searches, the name pattern, whether it skips inherited methods, the attributes a method carries one of, and
     * the fields it projects.
     *
     * @param list<string> $withAnyAttribute
     */
    public static function writeMethodSearch(
        ?string $class,
        ?string $descendantsOf,
        string $name,
        array $withAnyAttribute,
        int $fields,
        bool $declaredOnly,
    ): string {
        $writer = new PayloadWriter();
        $writer->writeU8(match (true) {
            $class !== null => self::METHOD_SEARCH_EXACT_CLASS,
            $descendantsOf !== null => self::METHOD_SEARCH_DESCENDANTS,
            default => self::METHOD_SEARCH_ANY_CLASS,
        });
        $target = $class ?? $descendantsOf;
        if ($target !== null) {
            $writer->writeBytes($target);
        }
        $writer->writeBytes($name);
        $writer->writeBoolean($declaredOnly);
        $writer->writeCount($withAnyAttribute);
        foreach ($withAnyAttribute as $attribute) {
            $writer->writeBytes($attribute);
        }
        $writer->writeU32($fields);

        return $writer->finish();
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writeCodebaseFindMethodsRequest(int $generation, string $search): array
    {
        $writer = self::createMessage(self::CODEBASE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8(self::FIND_METHODS);
        $writer->writeBytes($search);

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{ValueReader, int<0, 4294967295>}
     */
    public static function readCodebaseFindMethodsResponse(array $payload, int $generation, int $fields): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::CODEBASE_QUERY_RESPONSE) {
            throw new ProtocolException("Expected a codebase query response, received analyzer message {$kind}.");
        }
        if (
            $reader->readU64() !== $generation
            || $reader->readU8() !== self::FIND_METHODS
            || $reader->readU32() !== $fields
        ) {
            throw new ProtocolException('A projected method query response does not match its request.');
        }

        return [$reader, $reader->readCount(1_000_000)];
    }

    /** @return list<null|bool|int|float|string|list<mixed>> */
    public static function writeCodebaseListRequest(int $generation, int $operation, ?int $classLikeKind = null): array
    {
        $writer = self::createMessage(self::CODEBASE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8($operation);
        if ($operation === self::LIST_CLASS_LIKES) {
            $writer->writeU8($classLikeKind ?? self::ANY_CLASS_LIKE);
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<string>
     */
    public static function readCodebaseListResponse(
        array $payload,
        int $generation,
        int $operation,
        ?int $classLikeKind = null,
    ): array {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::CODEBASE_QUERY_RESPONSE) {
            throw new ProtocolException("Expected a codebase query response, received analyzer message {$kind}.");
        }
        if ($reader->readU64() !== $generation || $reader->readU8() !== $operation) {
            throw new ProtocolException('A codebase list response does not match its request.');
        }
        if ($operation === self::LIST_CLASS_LIKES && $reader->readU8() !== ($classLikeKind ?? self::ANY_CLASS_LIKE)) {
            throw new ProtocolException('A codebase list response contains the wrong class-like kind.');
        }

        $count = $reader->readCount(65_536);
        $names = [];
        for ($index = 0; $index < $count; ++$index) {
            $names[] = $reader->readBytes();
        }
        $reader->finish();

        return $names;
    }

    /**
     * @param list<string> $names
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeCodebaseExistenceRequest(int $generation, int $predicate, array $names): array
    {
        $writer = self::createMessage(self::CODEBASE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8(self::CHECK_EXISTENCE);
        $writer->writeU8($predicate);
        $writer->writeCount($names);
        foreach ($names as $name) {
            $writer->writeBytes($name);
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<bool>
     */
    public static function readCodebaseExistenceResponse(array $payload, int $generation, int $predicate): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::CODEBASE_QUERY_RESPONSE) {
            throw new ProtocolException("Expected a codebase query response, received analyzer message {$kind}.");
        }
        if (
            $reader->readU64() !== $generation
            || $reader->readU8() !== self::CHECK_EXISTENCE
            || $reader->readU8() !== $predicate
        ) {
            throw new ProtocolException('A codebase existence response does not match its request.');
        }

        $count = $reader->readCount(65_536);
        $results = [];
        for ($index = 0; $index < $count; ++$index) {
            $results[] = $reader->readBoolean();
        }
        $reader->finish();

        return $results;
    }

    /**
     * @param list<MemberIdentifier> $members
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeCodebaseMemberExistenceRequest(int $generation, int $predicate, array $members): array
    {
        $writer = self::createMessage(self::CODEBASE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8(self::CHECK_MEMBER_EXISTENCE);
        $writer->writeU8($predicate);
        $writer->writeCount($members);
        foreach ($members as $member) {
            $writer->writeBytes($member->class);
            $writer->writeBytes($member->member);
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<bool>
     */
    public static function readCodebaseMemberExistenceResponse(array $payload, int $generation, int $predicate): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::CODEBASE_QUERY_RESPONSE) {
            throw new ProtocolException("Expected a codebase query response, received analyzer message {$kind}.");
        }
        if (
            $reader->readU64() !== $generation
            || $reader->readU8() !== self::CHECK_MEMBER_EXISTENCE
            || $reader->readU8() !== $predicate
        ) {
            throw new ProtocolException('A codebase member-existence response does not match its request.');
        }

        $count = $reader->readCount(65_536);
        $results = [];
        for ($index = 0; $index < $count; ++$index) {
            $results[] = $reader->readBoolean();
        }
        $reader->finish();

        return $results;
    }

    /**
     * @param list<string> $names
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeCodebaseRelationsRequest(int $generation, int $relation, array $names): array
    {
        $writer = self::createMessage(self::CODEBASE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8(self::GET_CLASS_LIKE_RELATIONS);
        $writer->writeU8($relation);
        $writer->writeCount($names);
        foreach ($names as $name) {
            $writer->writeBytes($name);
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<list<string>>
     */
    public static function readCodebaseRelationsResponse(array $payload, int $generation, int $relation): array
    {
        [$kind, $reader] = self::readRequest($payload);
        if ($kind !== self::CODEBASE_QUERY_RESPONSE) {
            throw new ProtocolException("Expected a codebase query response, received analyzer message {$kind}.");
        }
        if (
            $reader->readU64() !== $generation
            || $reader->readU8() !== self::GET_CLASS_LIKE_RELATIONS
            || $reader->readU8() !== $relation
        ) {
            throw new ProtocolException('A codebase relation response does not match its request.');
        }

        $resultCount = $reader->readCount(65_536);
        $results = [];
        for ($resultIndex = 0; $resultIndex < $resultCount; ++$resultIndex) {
            $nameCount = $reader->readCount(65_536);
            $names = [];
            for ($nameIndex = 0; $nameIndex < $nameCount; ++$nameIndex) {
                $names[] = $reader->readBytes();
            }
            $results[] = $names;
        }
        $reader->finish();

        return $results;
    }

    /**
     * @param array<string, string|MemberIdentifier|ReferenceOrigin> $queries
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeReferenceQuery(int $generation, int $operation, array $queries): array
    {
        $writer = self::createMessage(self::SYMBOL_REFERENCE_QUERY_REQUEST);
        $writer->writeU64($generation);
        $writer->writeU8($operation);
        $writer->writeCount($queries);
        foreach ($queries as $query) {
            if ($operation === self::GET_REFERENCES_TO) {
                /** @var string|MemberIdentifier $query */
                self::writeSymbolIdentifier($writer, $query);
                continue;
            }

            $origin = $query instanceof ReferenceOrigin ? $query : ReferenceOrigin::symbol($query);
            self::writeReferenceOrigin($writer, $origin);
        }

        return $writer->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<list<SymbolReference>>
     */
    public static function readReferenceQueryResponse(
        array $payload,
        int $generation,
        int $operation,
        int $expectedCount,
    ): array {
        [$kind, $reader] = self::readRequest($payload);
        if (
            $kind !== self::SYMBOL_REFERENCE_QUERY_RESPONSE
            || $reader->readU64() !== $generation
            || $reader->readU8() !== $operation
        ) {
            throw new ProtocolException('A symbol-reference response does not match its request.');
        }

        $resultCount = $reader->readCount(1_000_000);
        if ($resultCount !== $expectedCount) {
            throw new ProtocolException('A symbol-reference response contains the wrong number of results.');
        }
        $results = [];
        for ($resultIndex = 0; $resultIndex < $resultCount; ++$resultIndex) {
            $referenceCount = $reader->readCount(1_000_000);
            $references = [];
            for ($referenceIndex = 0; $referenceIndex < $referenceCount; ++$referenceIndex) {
                $source = self::readReferenceOrigin($reader);
                $target = self::readSymbolIdentifier($reader);
                $referenceKind = ReferenceKind::tryFrom($reader->readU8());
                if ($referenceKind === null) {
                    throw new ProtocolException('A symbol reference has an unknown kind.');
                }

                $references[] = new SymbolReference($source, $target, $referenceKind);
            }
            $results[] = $references;
        }
        $reader->finish();

        return $results;
    }

    private static function writeReferenceOrigin(ValueWriter $writer, ReferenceOrigin $origin): void
    {
        if ($origin->file !== null) {
            $writer->writeU8(3);
            $writer->writeBytes($origin->file);
            return;
        }

        self::writeSymbolIdentifier($writer, $origin->symbol ?? '');
    }

    private static function writeSymbolIdentifier(ValueWriter $writer, string|MemberIdentifier $symbol): void
    {
        if ($symbol instanceof MemberIdentifier) {
            $writer->writeU8(2);
            $writer->writeBytes($symbol->class);
            $writer->writeBytes($symbol->member);
            return;
        }

        $writer->writeU8(1);
        $writer->writeBytes($symbol);
    }

    private static function readReferenceOrigin(ValueReader $reader): ReferenceOrigin
    {
        $kind = $reader->readU8();
        if ($kind === 3) {
            return ReferenceOrigin::file($reader->readBytes());
        }

        return ReferenceOrigin::symbol(self::readSymbolIdentifier($reader, $kind));
    }

    private static function readSymbolIdentifier(ValueReader $reader, ?int $kind = null): string|MemberIdentifier
    {
        return match ($kind ?? $reader->readU8()) {
            1 => $reader->readBytes(),
            2 => new MemberIdentifier($reader->readBytes(), $reader->readBytes()),
            default => throw new ProtocolException('A symbol reference has an unknown endpoint kind.'),
        };
    }

    /** @param int<0, 65535> $kind */
    private static function createMessage(int $kind): ValueWriter
    {
        return new ValueWriter([self::NAME, $kind]);
    }
}
