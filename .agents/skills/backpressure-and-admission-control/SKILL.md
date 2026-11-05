---
name: backpressure-and-admission-control
description: >-
  Design, implement, or review overload protection in APIs, workers, queues,
  streams, and downstream calls. Use whenever queues grow, memory rises during
  bursts, work backs up, a dependency is slow, or the user asks about bounded
  work, early rejection, 429/503 behavior, rate/concurrency limits, bulkheads,
  or tenant fairness—even if they do not say “backpressure.” Inspect the actual
  runtime and workload; do not treat an in-memory queue as durable processing.
---

# Backpressure and admission control

Protect a service by making its capacity and overload behavior explicit. The
goal is not to accept every request: it is to preserve useful work and bounded
latency when offered load exceeds sustainable capacity.

## Map the work before choosing a mechanism

Trace work from request admission through waiting, execution, downstream calls,
and response or durable acknowledgement. Identify every place work can wait,
including runtime mailboxes, async task creation, batch buffers, stream buffers,
client pools, and broker consumers. An unbounded queue hidden behind a bounded
queue is still unbounded work.

Distinguish:

- queued work from active/in-flight work;
- worker concurrency from downstream concurrency;
- arrival rate from service rate;
- per-process limits from shared/distributed limits;
- volatile accepted work from work durably persisted before acknowledgement.

Inspect runtime semantics and existing contracts before editing. For example,
Go channels, Tokio bounded `mpsc`, Node promises/queues, and OTP mailboxes do not
have interchangeable capacity behavior. In particular, never assume task or
mailbox creation is bounded merely because worker concurrency is bounded.

## Choose overload behavior deliberately

Set explicit limits for waiting work, active workers, downstream calls, batch
size, stream buffering, and per-tenant outstanding work where applicable. Bound
the total work-in-system, not only one visible queue. Size limits from memory,
latency objectives, and downstream capacity; ask for the relevant workload or
service-level objective when it cannot be inferred.

Choose the behavior at each full boundary:

- reject immediately when waiting would create unacceptable latency or consume
  unbounded memory;
- apply backpressure to the producer when the protocol and caller can honor it;
- shed lower-priority work only under an explicit priority policy;
- persist work before acknowledging acceptance when it must survive process
  failure.

Use response semantics that match the contract: `429` commonly represents a
client/request quota or admission limit; `503` commonly represents temporary
service or dependency saturation. Preserve established API contracts and include
retry guidance such as `Retry-After` only when it is meaningful. Do not silently
change response status or acceptance semantics.

For streaming, honor write-side flow control and cancellation rather than
materializing the complete response. For batches, bound both item count and
parallel operations. For tenant isolation, reserve and release capacity
atomically on every success, rejection, timeout, cancellation, and failure path.
A per-tenant quota is not a fair scheduler; state that distinction and propose
per-tenant queues or weighted scheduling only when the product needs it.

## Observe and test saturation

Expose low-cardinality measures that reveal the control loop: queue depth and
capacity, active workers, downstream slots, accepted/rejected/processed/failed
counts, timeouts, retries, and processing latency. Keep queue depth separate
from in-flight work. Avoid unbounded labels such as arbitrary user IDs.

Add tests that fill each bounded boundary, verify the expected rejection or
producer backpressure, ensure one tenant cannot consume another's reserved
capacity, and prove permits/counters are released on cancellation and errors.
Exercise slow downstreams and confirm the system does not accumulate unlimited
tasks, memory, or latency. A full queue can be the intended protective behavior;
look for uncontrolled growth, excessive tail latency, data loss, or unfairness
before calling it a defect.

## Durability boundary

Local queues and process memory are volatile. If accepted work must survive a
restart or machine loss, use durable persistence or a broker with explicit
acknowledgement and idempotent consumers; consider a transactional outbox when
the work originates from a database transaction. Do not describe a bounded
in-memory queue as a durable job system.

When performance is the reason for a change, use `performance-benchmarking-and-load-testing`
to reproduce saturation and `performance-optimization` to prove the change
improves the target without moving the failure elsewhere.
