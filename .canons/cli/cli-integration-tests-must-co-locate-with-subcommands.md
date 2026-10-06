---
inspect:
  - diff
tags:
  - testing
  - cli-ergonomics
---
CLI applications supporting multiple subcommands MUST co-locate subcommand-specific integration tests directly alongside their respective subcommand definition modules, reserving the root CLI integration test suite exclusively for global entrypoint dispatch, top-level flag routing, and process termination signals.

Rationale: Citing *Software Engineering at Google* (Ch. 11, "Testing Overview") and standard language test layouts (Go, Rust), co-locating integration tests with their target subcommand subsystem enforces spatial locality, prevents monolithic test bottleneck files, and enables targeted, low-latency test execution during focused development.
