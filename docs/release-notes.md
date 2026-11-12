CANHarbor local CAN/CAN FD workbench.

- Choose the macOS ARM64, Linux x86_64 or Windows x64 archive for your system. Other architectures have not been packaged by this workflow.
- Extract the archive. On macOS double-click `Start CANHarbor.command`; on Linux run `./start-canharbor.sh`; on Windows double-click `start-canharbor.cmd`. Keep the service window open.
- The default service provides two simulated channels. Use the first virtual receive/send walkthrough in the connection panel.
- Binary packages are unsigned and not notarized. macOS and Safari compatibility require release acceptance; real adapter compatibility is unverified.
- Each binary archive includes license notices and the source revision. Matching source archives and SHA-256 files are attached.

This release is prepared as a draft. Before publishing, review the checksums, verify all three packaged platforms, and record the tested macOS and browser versions. Signing/notarization requires the publisher's Apple credentials and a separate verified signing process. Do not describe an unsigned build as signed or validated.
