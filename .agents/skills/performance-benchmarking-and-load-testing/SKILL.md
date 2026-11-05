---
name: performance-benchmarking-and-load-testing
description: >-
  Design, implement, run, or interpret reproducible performance benchmarks and
  application load/capacity tests. Use whenever the user asks how many RPS a
  service can handle, wants a benchmark or before/after comparison, or mentions
  load, stress, saturation, spike, soak, ceiling, WebSocket, or streaming tests.
  Inspect the existing harness and choose workload and tools appropriate to the
  system; avoid unsafe production load and misleading cross-machine comparisons.
---

# Reproducible benchmarking and load testing

Design the experiment around a concrete question. A microbenchmark isolates a
small operation; a load test measures a service and its dependencies. Neither
alone proves production capacity. Prefer the repository's established tools,
scenarios, and result format.

## Choose workload and measurement model

Clarify the objective, endpoint/path, representative inputs and data size,
expected traffic mix, concurrency, duration, dependencies, target environment,
and acceptable latency/error thresholds. Ask before running costly, disruptive,
or production-targeted load when scope or authorization is unclear.

Choose the model deliberately:

- **Open-loop / constant arrival rate** keeps offered arrivals independent of
  response time and can expose queueing and saturation. Track dropped/started
  iterations and coordinated-omission risks.
- **Closed-loop / fixed users or connections** models clients that wait for
  responses, useful for sessions and concurrency, but can hide overload as the
  clients slow down.
- **Microbenchmark** isolates a hot path and should use the project's release
  mode, realistic setup, warmup, repeated samples, and allocation/throughput
  measures where supported.

Do not treat these models as interchangeable. Include smoke, steady-state,
capacity/ceiling, burst or spike, soak/endurance, and distributed scenarios only
when they answer the product question. Exercise realistic mixes and payloads;
test websocket sessions, streaming, fan-out, or tenant fairness when those are
actual product behaviors.

## Keep the experiment interpretable

- Pin or record tool/runtime versions, commit, build mode/flags, hardware and
  architecture, OS/kernel, CPU/memory limits, topology, data seed, parameters,
  warmup, duration, and repetitions.
- Isolate test dependencies and use deterministic fixtures when appropriate.
  Verify the fixture itself is not the bottleneck; distinguish mock ceilings
  from representative real-dependency results.
- Avoid concurrent benchmark suites on shared hardware unless contention is the
  subject. Pin application and load-generator CPUs separately only when the
  environment and comparison goal justify it.
- Measure the load generator and dependency resources as well as the target.
  Record offered versus delivered rate, latency percentiles, errors/statuses,
  CPU, memory, queue depth/age, and dependency activity relevant to the claim.
- Warm caches intentionally and document whether a scenario is cold, warm, or
  mixed. Reset or recreate state between phases when earlier writes contaminate
  later measurements.

Define a healthy capacity step before running. A ceiling is the last step that
meets agreed thresholds; report the first unhealthy step and why it failed.
Throughput alone is insufficient if tail latency, error rate, delivery ratio,
backlog, or resource consumption is unacceptable. Treat the load tool's own
limits as a possible confounder, not as an application ceiling.

## Reproducibility, artifacts, and safety

Create a repeatable script/target with validated parameters, bounded durations,
clear prerequisites, cleanup traps, and a unique result directory. Keep raw
profiles/logs separate from concise summaries. Prefer structured JSON with a
versioned schema plus a readable Markdown report; record only allowlisted
configuration/environment fields instead of dumping the full environment or
process command lines. Keep secrets, request bodies, personal paths, hostnames,
and raw profile artifacts out of committed reports unless deliberately reviewed
and approved. Ignore generated bulk results by default and document which
summaries are suitable for version control.

Do not run destructive cleanup against shared services or production data. Use
dedicated test resources and clearly scoped ports/names. Never claim a local
loopback result predicts production without representative network, TLS,
architecture, and dependency validation.

## Compare campaigns

Run baseline and candidate with the same workload and environment, preferably
as multiple independent runs. Compare medians/distributions and confidence in
the change, not a single best sample. Evaluate the agreed target plus guardrails
for p95/p99, errors, resource use, queue stability, and dependency/cost impact.
Record capped results as lower bounds rather than exact ceilings. Document
inconclusive, failed, and unsupported runs instead of silently dropping them.

For code changes driven by the result, follow `performance-optimization`. For
runtime or eBPF diagnosis, follow `performance-profiling-ebpf`.
