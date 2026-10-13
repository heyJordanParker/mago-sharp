<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\Analyzer\Declaration\ClassLikeRefinement;
use Mago\Sdk\Analyzer\Declaration\FunctionLikeRefinement;
use Mago\Sdk\CancellationTokenInterface;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\PHPVersion;

/**
 * A deterministic batch of source files selected during codebase scanning.
 *
 * Clear derived state when `firstBatch` is true. Provider requests only begin
 * after the batch for which `lastBatch` is true has returned.
 *
 * @api
 */
final class CodebaseScanContext
{
    /** @var list<ClassLikeRefinement|FunctionLikeRefinement> */
    private array $refinements = [];

    /**
     * @param list<CodebaseScanFile> $files
     * @internal
     */
    public function __construct(
        public readonly PHPVersion $phpVersion,
        public readonly CancellationTokenInterface $cancellation,
        public readonly array $files,
        public readonly bool $firstBatch,
        public readonly bool $lastBatch,
    ) {}

    /**
     * Refines a class-like or function-like declared in one of this batch's files.
     *
     * Every worker receives the same batch and must describe it identically.
     */
    public function refine(ClassLikeRefinement|FunctionLikeRefinement $refinement): void
    {
        foreach ($this->files as $file) {
            if ($file->path !== $refinement->file) {
                continue;
            }

            $this->refinements[] = $refinement;

            return;
        }

        throw new InvalidArgumentException("A refinement names `{$refinement->file}`, which is not in this batch.");
    }

    /**
     * @return list<ClassLikeRefinement|FunctionLikeRefinement>
     * @internal
     */
    public function getRefinements(): array
    {
        return $this->refinements;
    }
}
