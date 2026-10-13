<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\CancellationTokenInterface;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Internal\Analyzer\CodebaseReads;
use Mago\Sdk\Internal\Analyzer\MetadataCache;
use Mago\Sdk\Internal\Analyzer\Protocol;
use Mago\Sdk\Internal\HostClient;

use function array_fill;
use function array_values;
use function count;

/**
 * Performs codebase-aware type comparisons using Mago's native type system.
 *
 * @api
 * @mago-expect lint:cyclomatic-complexity
 */
final class TypeComparator
{
    private const MAXIMUM_COMPARISONS = 65_536;

    /** @var array<string, array{bool, list<string>}> */
    private array $answers = [];

    /**
     * `$reads` records a class-level read of every class-like a comparison names, cached or not, when the comparator
     * serves one file's hooks or providers.
     *
     * @param positive-int $requestId
     * @internal
     */
    public function __construct(
        private readonly HostClient $host,
        private readonly int $requestId,
        private readonly CancellationTokenInterface $cancellation,
        private readonly ?MetadataCache $cache = null,
        private readonly ?CodebaseReads $reads = null,
    ) {}

    public function equals(Type $left, Type $right): bool
    {
        return $this->compareMultiple([TypeComparison::equal($left, $right)])[0];
    }

    public function isContainedBy(Type $input, Type $container): bool
    {
        return $this->compareMultiple([TypeComparison::containedBy($input, $container)])[0];
    }

    public function canBeIdentical(Type $left, Type $right): bool
    {
        return $this->compareMultiple([TypeComparison::canBeIdentical($left, $right)])[0];
    }

    /**
     * Evaluates multiple, potentially different type relationships in one native request.
     *
     * @param list<TypeComparison> $comparisons
     *
     * @return list<bool>
     *
     * @mago-expect analysis:impossible-condition Runtime validation protects untyped callers.
     */
    public function compareMultiple(array $comparisons): array
    {
        $count = count($comparisons);
        if ($count === 0) {
            return [];
        }

        if ($count > self::MAXIMUM_COMPARISONS) {
            throw new InvalidArgumentException('A type-comparison batch cannot contain more than 65,536 comparisons.');
        }

        $results = array_fill(0, $count, false);
        $pending = [];
        $positions = [];
        $position = 0;
        foreach ($comparisons as $comparison) {
            if (!$comparison instanceof TypeComparison) {
                throw new InvalidArgumentException('A type-comparison batch must contain only TypeComparison values.');
            }

            $key = $comparison->cacheKey();
            $cached = $this->cached($comparison, $key);
            if ($cached !== null) {
                $this->recordClassLikes($cached[1]);
                $results[$position++] = $cached[0];
                continue;
            }

            $pending[$key] ??= $comparison;
            $positions[$key][] = $position++;
        }

        if ($pending === []) {
            return $results;
        }

        $this->cancellation->throwIfCancelled();
        $requests = array_values($pending);
        $requestCount = count($requests);
        $comparison = $requests[0];
        $payload = $requestCount === 1
            ? Protocol::writeTypeComparisonRequest(
                $comparison->kind->value,
                $comparison->encodeLeft(),
                $comparison->encodeRight(),
            )
            : Protocol::writeTypeComparisonBatchRequest($requests);
        $response = $this->host->request($this->requestId, $payload);
        $this->cancellation->throwIfCancelled();
        $answers = $requestCount === 1
            ? [Protocol::readTypeComparisonResponse($response)]
            : Protocol::readTypeComparisonBatchResponse($response, $requestCount);
        $index = 0;
        foreach ($pending as $key => $comparison) {
            $answer = $answers[$index++];
            $this->remember($comparison, $key, $answer);
            $this->recordClassLikes($answer[1]);
            foreach ($positions[$key] as $resultPosition) {
                $results[$resultPosition] = $answer[0];
            }
        }

        return $results;
    }

    /** @return array{bool, list<string>}|null */
    private function cached(TypeComparison $comparison, string $key): ?array
    {
        if (
            !$comparison->left->isRequestReference()
            && !$comparison->right->isRequestReference()
            && $this->cache !== null
        ) {
            return $this->cache->typeComparisons[$key] ?? null;
        }

        return $this->answers[$key] ?? null;
    }

    /** @param array{bool, list<string>} $answer */
    private function remember(TypeComparison $comparison, string $key, array $answer): void
    {
        if (
            !$comparison->left->isRequestReference()
            && !$comparison->right->isRequestReference()
            && $this->cache !== null
        ) {
            $this->cache->typeComparisons[$key] = $answer;
            return;
        }

        $this->answers[$key] = $answer;
    }

    /** @param list<string> $classLikes the class-likes one answered comparison named */
    private function recordClassLikes(array $classLikes): void
    {
        foreach ($classLikes as $classLike) {
            $this->reads?->record(Protocol::GET_CLASS_LIKES, 0, $classLike);
        }
    }
}
