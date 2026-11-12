# Binary packages

The manually triggered **Package** workflow builds macOS ARM64, Linux x86_64 and Windows x64
archives with the browser workbench embedded. It uploads workflow artifacts;
without a release tag it does not create a release. Supplying an existing `v*`
tag to the workflow builds that exact tagged commit and prepares a **draft**
release containing all three platform binaries, matching source and checksums. It
refuses to overwrite an already published release. Publication remains an
explicit maintainer action after acceptance. Download the archive for your platform from a
successful workflow run, extract it, then double-click `Start CANHarbor.command` on macOS or run
`./start-canharbor.sh` on Linux, or double-click `start-canharbor.cmd` on Windows. The launcher reuses an existing default service
or starts one in the foreground; keep that window open. CLI alternative:

```sh
./canharbor serve --open
```

No Rust, Bun or Node.js installation is needed to run the binary. The default
service exposes two virtual channels. Hardware access remains opt-in and
unverified. Packages are unsigned and are not notarized; this workflow does not
establish compatibility with every macOS version or adapter.

Each binary archive includes the license, notices and a source revision. The
workflow also supplies a source archive from that same commit and SHA-256 files.
Before uploading, each platform verifies checksums, all source bytes and file
modes against the checked-out commit, binary architecture, licenses and launcher
permissions. It extracts the package into a path containing spaces and runs the
extracted CLI on its native runner: an isolated service on a random loopback
port, embedded HTML/JavaScript/CSS, launcher reuse and browser pairing, one
virtual CAN exchange, session cleanup and graceful shutdown. On Unix the OS browser opener is stubbed; on Windows the launcher
uses `--print` to return its pairing URL without opening a browser; the test does not access physical adapters or open a
desktop browser. A failed verification blocks that platform's artifact upload
and the dependent draft-release job.
Compare the checksum with your download before running it. Workflow artifacts
have limited retention; they are not a permanent release channel.

The read-only `package-build.yml` workflow is shared by PR/main CI and Package.
Release mutation remains in Package, keeping build-only CI tokens read-only.
The target manifest (`scripts/package-targets.json`) defines all expected archives;
missing platforms cannot disappear from verification by omitting downloaded files.

## Draft release rehearsal

Manually run **Draft release rehearsal** (`draft-release-check.yml`) to test the
complete draft path without publishing. It pins the selected workflow commit
with a temporary annotated `v0.0.0-draft-check-<run>-<attempt>` tag and calls the
same Package workflow, including all three native archive/smoke checks. It then
requires an unpublished draft with exactly eight assets: three platform archives,
one source archive and their four SHA-256 files. Downloaded assets are checked
against GitHub's digests, the tested workflow artifacts and the tagged source.

An independent `always()` cleanup job runs after success, build/verification
failure or ordinary cancellation. It uses the exact tag object recorded before
ref creation, checks the run ownership marker and target commit, and confirms
that only its draft and tag are gone. It refuses to delete a published release,
a pre-existing object or a tag that has moved. Force-cancelling the entire run
or a runner outage can interrupt cleanup; inspect the cleanup job/summary and
finish cleanup before abandoning a failed rehearsal. The workflow never
publishes, and does not accept an arbitrary release/tag name.

## Local packaging

Build in a clean checkout using the pinned toolchain and Python 3.11 or later:

```sh
bun install --cwd web --frozen-lockfile
bun run --cwd web build
cargo build --release -p canharbor-cli --features web-ui,slcan --locked
python3 scripts/package.py --platform macos-arm64
```

Use `linux-x86_64` on Linux or `windows-x86_64` on Windows. Windows builds use
the MSVC toolchain and produce `target/release/canharbor.exe` in a ZIP. For a portable
Windows release, set `$env:RUSTFLAGS = '-C target-feature=+crt-static'` in PowerShell
before the release build. Package CI sets this flag and audits normal PE imports;
VC redistributable/third-party DLL dependencies and delayed imports fail validation.
Only Windows system DLLs/API sets may remain dynamic. Run
`python scripts/verify-package.py --platform windows-x86_64` after packaging. Outputs are written to `dist/packages/`.
Packaging refuses source modifications or untracked files to keep the archived source reproducible. A clean checkout cannot prove that a
local binary was built from that commit; rebuild immediately before packaging. Use a fresh checkout so no source files are omitted.
The packager does not sign binaries, publish artifacts or include vendor libraries.

## Release acceptance

Published versions belong on [Releases](https://github.com/soulteary/canharbor/releases),
which is the stable download entry. Workflow artifacts remain a development fallback.
No published version or supported macOS release is asserted by this source tree.
Before publishing a draft, verify the extracted launcher, browser pairing,
virtual receive/send and clean shutdown on all three packaged platforms, and record
the OS and browser versions. CI runs Chromium; set `CANHARBOR_BROWSER=webkit`
for a separate WebKit check. WebKit automation does not replace Safari acceptance.

The workflow currently produces unsigned packages. Apple signing and notarization
need the publisher's certificate, notarization credentials and a tested release
process. These external prerequisites cannot be satisfied by source changes alone.
