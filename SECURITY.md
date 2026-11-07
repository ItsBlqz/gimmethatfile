# Security Policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report them privately through GitHub:
**Security → Report a vulnerability** on this repository
([direct link](../../security/advisories/new)).

Include what you found, how to reproduce it (a URL or a small server
configuration that triggers it is ideal), and the impact you expect. We aim to
acknowledge reports within 7 days and will keep you updated until a fix is
released. We're happy to credit you in the release notes.

## Scope

Areas we especially care about:

- A server making the app write outside the chosen download folder, for example
  through a hostile `Content-Disposition` file name or redirects.
- A server making the app run or open a file without the user asking.
- Corrupted downloads reported as complete.
- Anything that lets web content in the UI reach Tauri commands it shouldn't.

Downloaded files themselves are untrusted. Gimme That File doesn't scan them,
so scan files from sources you don't trust.
