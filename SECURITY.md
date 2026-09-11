# Security policy

MultiVoice records microphone audio, writes to the clipboard, simulates
keystrokes to insert text, and (when context awareness is enabled) reads the
focused app's Accessibility tree. We take reports about any of these seriously.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's
**[Report a vulnerability](../../security/advisories/new)** form on this
repository (Security → Advisories). Do not open a public issue or pull request
for a security problem.

Include what you can of:

- the affected version or commit,
- your OS and version,
- steps to reproduce or a proof of concept,
- the impact you believe it has.

We aim to acknowledge reports within 3 business days and to agree a disclosure
timeline with you once the issue is confirmed.

## Supported versions

Security fixes are made against the latest release and `main`.

## Scope

In scope: the desktop app and the bundled `transcription-host` binary in this
repository. The hosted MultiVoice Cloud service is operated separately; report
issues with it through the same form and we will route them.
