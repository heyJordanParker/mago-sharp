+++
title = "Worker state and reduction"
description = "Merge interpreter-local extension data when Mago stops the hosts."
nav_order = 40
nav_section = "Extensions"
nav_subsection = "PHP SDK"
+++
# Worker state and reduction

Each analysis thread's interpreter has independent memory. `WorkerReducer` lets an extension collect one opaque payload per interpreter and perform terminal work with the complete ordered set.

Use reduction for extension-owned output such as metrics, indexes, or a single API upload. It is not an analyzer hook and cannot add diagnostics to a completed Mago result.

## Implement a reducer

Place worker-level support such as reducers under `src/Mago/Worker/`:

```php
<?php

declare(strict_types=1);

namespace Acme\Mago\Worker;

use Mago\Sdk\WorkerReducer;
use Mago\Sdk\WorkerReductionContext;

final class CoverageReducer implements WorkerReducer
{
    /** @var array<string, int> */
    private array $covered = [];

    public function record(string $file, int $expressions): void
    {
        $this->covered[$file] = $expressions;
    }

    public function collect(): string
    {
        return json_encode($this->covered, JSON_THROW_ON_ERROR);
    }

    public function reduce(WorkerReductionContext $context): void
    {
        $merged = [];
        foreach ($context->workerPayloads as $payload) {
            foreach (json_decode($payload, true, flags: JSON_THROW_ON_ERROR) as $file => $count) {
                $merged[$file] = $count;
            }
        }

        file_put_contents('build/type-coverage.json', json_encode($merged, JSON_THROW_ON_ERROR));
    }
}
```

The payload is an opaque byte string to Mago. JSON is convenient, not required; extensions may use a compact binary representation.

## Share the reducer with callbacks

Construct one reducer object and pass the same instance to rules or plugins that record data:

```php
$coverage = new CoverageReducer();

new Extension(
    identifier: 'acme/coverage',
    name: 'Acme type coverage',
    version: '1.0.0',
    analyzerPlugins: [new CoveragePlugin($coverage)],
    workerReducer: $coverage,
);
```

The object is shared only within one interpreter. Each interpreter constructs its own equivalent object graph.

## Shutdown sequence

When reduction is registered, Mago, once it stops the hosts:

```mermaid
sequenceDiagram
    participant Mago as Mago
    participant I1 as Interpreter 1
    participant I2 as Interpreter 2
    participant IT as Interpreter T

    par Collect on each analysis thread
        Mago->>I1: collect()
        I1-->>Mago: opaque payload 1
    and
        Mago->>I2: collect()
        I2-->>Mago: opaque payload 2
    and
        Mago->>IT: collect()
        IT-->>Mago: opaque payload T
    end
    Mago->>I1: reduce([payload 1, 2, ..., T])
    I1-->>Mago: reduction complete
```

1. calls `collect()` in every interpreter that served the host, each on its own thread;
2. keeps the collection responses in thread order;
3. calls `reduce()` for each extension reducer in one interpreter, with every response.

`WorkerReductionContext::$workerPayloads` is therefore a non-empty list. It includes the reducing interpreter's own payload. The order is stable but should not be interpreted as source-file order.

Reduction is enabled only for hosts advertising at least one reducer, so ordinary extensions pay no collection or reduction request cost.

## Failure and deadlines

Both methods may throw. A collection or reduction failure is logged as a worker-reduction warning; it does not replace the completed lint or analysis result.

Both `collect()` and `reduce()` use the host's `request-timeout-ms`. A method that runs past it is interrupted, and the reduction fails with that warning.

## Design guidance

- Keep `collect()` deterministic and fast; it runs once in every interpreter.
- Send the minimum state required for the final operation.
- Make externally visible reduction effects idempotent when possible.
- Do not depend on reduction for correctness of Mago's reported analysis.
