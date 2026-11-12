# Stress testing

Stress tasks run in the local service, independently of browser rendering. Plans
are validated before explicit transmission authorization. Hardware compatibility
and physical performance remain unverified; the examples below use virtual CAN.

## Run a virtual plan

Build the application as described in the [README](../README.md). Start an
isolated service with no hardware selection arguments:

```sh
./target/debug/canharbor --runtime-dir /tmp/canharbor-stress-demo serve --open
```

In another terminal:

```sh
./target/debug/canharbor --runtime-dir /tmp/canharbor-stress-demo stress validate --plan examples/stress/virtual-classic.json
./target/debug/canharbor --runtime-dir /tmp/canharbor-stress-demo stress run --plan examples/stress/virtual-classic.json --active --request-id demo-001
./target/debug/canharbor --runtime-dir /tmp/canharbor-stress-demo stress report RUN_UUID --output /tmp/stress-report.json
./target/debug/canharbor --runtime-dir /tmp/canharbor-stress-demo stress delete RUN_UUID
```

Replace `RUN_UUID` with the returned run ID. Report export creates a new file and
refuses to overwrite an existing destination. The browser supports plan
validation, explicit start, status, stop, report and evidence downloads.

The same request ID and unchanged plan recover the original run instead of
executing it again. If the creation response is lost, retain that request ID.
CLI disconnection does not cancel an authorized service task; use `stress status`
and `stress stop` to manage it. Ctrl-C requests a stop. No automatic restart or
unknown-outcome retry is performed.

## Plans and reports

Fixed-count, staged and soak plans freeze configuration, resource budgets and
criteria before execution. Configuration changes require a new validation hash.
The service reserves the required device set before starting acquisition;
partial setup failure rolls back only resources created by that attempt.

Reports distinguish scheduled slots, known submissions, unknown outcomes, receiver
observations, evidence completeness and verdict. Skipped slots do not count as
hardware loss. TX echo does not count as peer RX. A completed driver call is not
proof of physical delivery. Tolerances cannot turn stopped, interrupted or
incompletely observed runs into a pass.

See the feature references:

- [Criteria and verdicts](stress-criteria.md)
- [Timing and software provenance](stress-timing.md)
- [Bidirectional multi-flow plans](stress-multiflow.md)
- [Classic patterns and checkpoints](stress-pattern.md)
- [CAN FD, CRC, RTT and multi-host coordination](stress-protocols.md)
- [Adaptive plans and load shapes](stress-adaptive.md)
- [Tracking windows, queues and benchmark evidence](stress-tracking.md)
- [Stopping, persistence and read-only recovery](stress-recovery.md)

## API

The control surface includes `POST /api/v1/stress/validate`,
`POST /api/v1/stress/runs`, `GET /api/v1/stress/runs`,
`GET /api/v1/stress/requests/{request}`, `GET/DELETE /api/v1/stress/runs/{id}`,
`POST /api/v1/stress/runs/{id}/stop` and report/artifact downloads below the run.
Browser mutations require the existing pairing, Origin and CSRF checks. Native
same-user clients can administer runs; browser access is scoped to its identity.
See the generated schema for payloads and the CLI help for command arguments.

## Verification limits

Software tests use virtual CAN, simulated drivers and local services. Stored
benchmark records include failures and do not establish sustained adapter
throughput. Real-time long runs, physical error injection and external-host
coordination require separate acceptance. See [implementation status](status.md).

## Workbench templates

Short receive check, Rate staircase and One-minute soak reset the editable plan
to a bounded Classic CAN setup. They preserve channel selection, clear prior
validation and transmission authorization, and do not start a run. Review the
sender and expected receivers before validating. Advanced criteria, tracking,
host observations and concurrent flows are collapsed by default; their values
remain part of the plan while closed.
