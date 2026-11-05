---
name: testing-standard
description: >-
  Apply the repository testing standard whenever adding or changing code, tests,
  persistence, APIs, CI, or fixing a bug, and whenever asked to verify that work
  is complete. Select meaningful unit, integration, and regression coverage;
  run the repository's full documented quality gate using safe local resources;
  report exactly what passed, failed, or was not run. Inspect repository
  instructions and commands instead of assuming a universal stack.
---

# Repository testing standard

Use this skill as the working standard for planning, writing, running, or
reviewing tests and for verifying code changes. Tests should demonstrate that
the relevant behavior works, not merely that mocks agree with implementation.
Follow the repository's documented conventions and actual CI; do not impose a
universal language, runner, directory layout, or infrastructure choice.

## Establish the local test contract

Before acting, inspect the applicable `AGENTS.md`, `TESTING_STANDARD.md`, build
and package configuration, CI workflows, test targets, test layout, and local
service definitions. Use exact commands and prerequisites from those sources.
Do not guess commands, tags, coverage thresholds, or infrastructure setup. If a
required detail is missing, identify the gap and ask or report it instead of
pretending the check was verified.

When creating or updating a repository's `TESTING_STANDARD.md`, document the
verified runtime versions, local dependencies and startup, test isolation and
cleanup, exact checks, integration-test opt-ins, and approved exceptions. Remove
illustrative placeholders or mark unresolved items clearly. Keep the document
aligned with CI and local setup; report contradictions rather than silently
choosing a source of truth.

## Select test boundaries

### Unit tests

- Exercise component logic in isolation.
- Mock only dependencies that cannot reasonably run locally, such as an
  unavailable external SaaS. At a protocol boundary, prefer testing the real
  production client against a local fake server.
- Cover relevant success, validation, error, timeout/retry, malformed or missing
  response, and partial-failure branches.
- Keep tests deterministic; inject clocks and other sources of nondeterminism.

### Integration tests

- Exercise real application wiring and concrete adapters.
- Run infrastructure that can run locally (database, cache, broker, filesystem,
  or sibling service) for real. Do not replace an available dependency with an
  in-memory substitute in an integration test.
- For changed routes or handlers, exercise the real router/framework and assert
  authorization outcomes. For changed repositories or DAOs, verify persisted
  state by reading it back from the real store.
- Cover transactions and rollback, serialization round trips, side effects,
  and concurrency invariants where relevant.
- Fake unavailable external services only at their protocol boundary; use the
  production client against the fake and verify requests and failure mapping.
- Never call development, staging, or production services from local tests.
- Use unique fixtures, isolate tests, clean up test data, and make suites safe to
  repeat and run concurrently.
- When stateful or order-dependent failures are plausible, run the documented
  integration suite repeatedly (typically 2–3 consecutive runs) and report that
  evidence.

### Regression tests

- Every bug fix includes a regression test that fails without the fix and passes
  with it.
- Reproduce the relevant failure mode before changing implementation when
  practical. Assert the actual contract or error, not a paraphrase.

## Run the complete quality gate

Before declaring a code change complete, run the applicable checks for the whole
repository, not only edited files. Use the repository's exact commands and fresh
or uncached runs when supported:

1. Format check.
2. Lint or static analysis.
3. Build or typecheck.
4. Full unit suite.
5. Full integration suite against real local dependencies when provided.
6. Coverage, security, or other checks required by the repository's standard
   and CI.

If a check is not applicable, document why. Do not silently skip an existing
integration suite. Do not start services, access external systems, or perform
deployment operations unless authorized and necessary for the requested
verification.

### Results and failures

- Fix failures caused by the change.
- Report pre-existing failures with the exact command and a concise, redacted
  summary; do not conceal them.
- If the user or repository policy explicitly authorizes whole-repository
  mechanical cleanup, apply narrowly scoped behavior-preserving fixes and
  recheck them. A green-gate requirement alone does not authorize unrelated
  behavioral changes, architecture work, or weakening assertions.
- Never claim the full suite passed when only a subset ran or when relying on
  cached results.
- Do not claim completion while required checks are unverified unless the
  repository owner explicitly approves an exception.
- Re-run the required gate after the final code change before reporting the
  result.

## Coverage policy

Treat coverage as evidence, not a target to game. Follow the repository's
documented threshold; do not invent one. Exclusions may cover generated code,
entrypoints, and mechanical assembly/bootstrap wiring only when justified and
documented. Do not exclude business logic, authorization, validation, handlers,
repositories, adapters, error handling, or retry behavior to inflate a
percentage.

## Safe test execution

- Use only local/test resources and explicitly test-scoped configuration.
- Never read, print, log, or commit resolved secrets. Do not source production
  environment files to run tests.
- Never apply migrations to shared or production databases during local
  verification.
- Use the repository-approved migration generator; do not hand-create
  migrations when the repository requires a generator.
- Keep test data unique and clean it up so repeated runs do not damage shared
  state or depend on run order.

## Privacy for reusable material

When this standard is copied into a public skill, example, or document, retain
only portable testing principles. Remove organization/product/customer names,
private repository paths, issue keys, board IDs, internal URLs, hostnames,
service topology, and operational details. Never include credentials, tokens,
private customer data, or resolved environment values. Use synthetic names and
redacted examples, then search the final files for source-specific identifiers
before publishing.

## Completion summary

State which checks ran and their results, which checks were skipped and why, and
any remaining failures or risks. Include exact command names where useful, but
redact private paths, hostnames, identifiers, and sensitive output. Never imply
that unrun checks passed.
