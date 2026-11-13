# Native ARM collector validation — 2026-11-13

Reviewed source `e98ef453b7b2ad873c7ecd6d47791b7de01b5e79`; actual default merge base `301241bf41e8ba8bb18d981f4e9138fe220e9c18`.

GitHub run [43389399355](https://github.com/hmumixaM/monica-rs/actions/runs/43389399355) passed backend, Web, macOS, adapters and AMD64 optional images. The ARM64 image built, but its offline Chromium process failed under cross-architecture QEMU with repeated GPU subprocess launch failures and `GPU process isn't usable`. This is not evidence that native ARM execution fails or passes.

Run each optional image build and its unchanged offline browser acceptance on a matching standard runner: `ubuntu-24.04` for AMD64 and `ubuntu-24.04-arm` for ARM64. Remove emulation from this job. Both architectures still build and run the real image, help entry points and locked offline Chromium. No acceptance is skipped, retried or relaxed; no production code, browser arguments or resource limit changes. GitHub documents [standard ARM runners for private repositories](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

Code-review inspected matrix expansion and platform/runner correspondence. Excessive-tests: existing two architecture acceptance cases remain distinct; no new duplicate test. Production artifacts and full platform matrices remain frozen at e98ef45; this later CI-only commit does not change delivered application source. Fresh CI outcome is recorded in the consolidated acceptance manifest.
