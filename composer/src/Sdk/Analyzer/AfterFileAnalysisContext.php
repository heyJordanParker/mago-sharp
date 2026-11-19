<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\PHPVersion;

/**
 * Context passed after one file has completed analysis.
 *
 * @api
 */
final class AfterFileAnalysisContext extends LifecycleContext
{
    /**
     * References discovered from this file.
     *
     * Mago replaces this file's previous contributions whenever the file is reanalyzed.
     */
    public readonly ReferenceRegistry $references;

    public function __construct(
        PHPVersion $phpVersion,
        Codebase $codebase,
        TypeComparator $types,
        public readonly FileAnalysis $analysis,
    ) {
        parent::__construct($phpVersion, $codebase, $types);

        $this->references = new ReferenceRegistry();
    }
}
