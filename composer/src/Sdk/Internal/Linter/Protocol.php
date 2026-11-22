<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Linter;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Extension;
use Mago\Sdk\Internal\Protocol\ValueReader;
use Mago\Sdk\Internal\Protocol\ValueWriter;
use Mago\Sdk\Internal\RuleTimes;
use Mago\Sdk\Internal\Syntax\SourceFileCodec;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Syntax\NodeKind;

use function count;

/**
 * @internal
 */
final class Protocol
{
    /** The name every linter message starts with. */
    public const NAME = 'linter';

    public const DESCRIBE_REQUEST = 1;
    public const LINT_FILE_REQUEST = 2;

    private const DESCRIBE_RESPONSE = 0x8001;
    private const LINT_FILE_RESPONSE = 0x8002;
    private const MAXIMUM_ISSUES = 1_000_000;

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{int<0, 65535>, ValueReader}
     */
    public static function readRequest(array $payload): array
    {
        $reader = new ValueReader($payload);
        if ($reader->readBytes() !== self::NAME) {
            throw new ProtocolException('The message is no linter message.');
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

    /**
     * @param non-empty-list<Extension> $extensions
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeDescribeResponse(array $extensions): array
    {
        $writer = self::createMessage(self::DESCRIBE_RESPONSE);
        $writer->writeCount($extensions);
        foreach ($extensions as $extension) {
            $writer->writeString($extension->identifier);
            $writer->writeString($extension->name);
            $writer->writeString($extension->version);
            $writer->writeBoolean($extension->workerReducer !== null);
            $writer->writeCount($extension->linterRules);
            foreach ($extension->linterRules as $rule) {
                $definition = $rule->getDefinition();
                $writer->writeString($definition->code);
                $writer->writeString($definition->name);
                $writer->writeString($definition->description);
                $writer->writeU8($definition->defaultLevel->value);
                $writer->writeBoolean($definition->defaultEnabled);
                $writer->writeCount($definition->targets);
                foreach ($definition->targets as $target) {
                    $writer->writeString($target->value);
                }
            }
        }

        return $writer->finish();
    }

    /**
     * @param list<NodeKind> $kinds
     */
    public static function readLintRequest(ValueReader $reader, PHPVersion $phpVersion, array $kinds): LintRequest
    {
        $file = $reader->readBytes();
        $source = $reader->readBytes();
        $activeRules = self::readActiveRules($reader);
        $sourceFile = SourceFileCodec::readValues($reader, $phpVersion, $kinds, $file, $source);

        $reader->finish();

        return new LintRequest($activeRules, $sourceFile);
    }

    /**
     * @return list<int<0, 65535>>
     */
    private static function readActiveRules(ValueReader $reader): array
    {
        $activeRules = [];
        $count = $reader->readU16();
        for ($index = 0; $index < $count; ++$index) {
            $activeRules[] = $reader->readU16();
        }

        return $activeRules;
    }

    /**
     * @param list<int<0, 65535>|Issue> $reportedIssues Flat rule index and issue pairs.
     * @param RuleTimes|null $ruleTimes how long each rule's calls took, when mago asked
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeLintResponse(array $reportedIssues, ?RuleTimes $ruleTimes): array
    {
        $valueCount = count($reportedIssues);
        $issueCount = $valueCount >> 1;
        if ($issueCount > self::MAXIMUM_ISSUES) {
            throw new ProtocolException('A linter response contains too many issues.');
        }

        $values = [self::NAME, self::LINT_FILE_RESPONSE, $issueCount];
        for ($index = 0; $index < $valueCount; $index += 2) {
            /** @var int<0, 65535> $ruleIndex */
            $ruleIndex = $reportedIssues[$index];
            /** @var Issue $issue */
            $issue = $reportedIssues[$index + 1];
            $values[] = $ruleIndex;
            $values[] = $issue->message;
            $values[] = count($issue->notes);
            foreach ($issue->notes as $note) {
                $values[] = $note;
            }

            $values[] = $issue->help;
            $values[] = $issue->link;
            $values[] = count($issue->annotations);
            foreach ($issue->annotations as $annotation) {
                $values[] = $annotation->kind->value;
                $values[] = $annotation->span->start;
                $values[] = $annotation->span->end;
                $values[] = $annotation->message;
            }

            $values[] = count($issue->edits);
            foreach ($issue->edits as $edit) {
                if ($edit->file !== null) {
                    throw new ProtocolException('A linter text edit cannot target another file.');
                }

                $values[] = $edit->span->start;
                $values[] = $edit->span->end;
                $values[] = $edit->safety->value;
                $values[] = $edit->newText;
            }
        }

        $writer = new ValueWriter($values);
        ($ruleTimes ?? new RuleTimes())->write($writer);

        return $writer->finish();
    }

    /**
     * @param int<0, 65535> $kind
     */
    private static function createMessage(int $kind): ValueWriter
    {
        return new ValueWriter([self::NAME, $kind]);
    }
}
