# Security Policy

## Reporting a vulnerability

Please do not report security vulnerabilities in a public issue.

Use GitHub's private security advisory workflow for this repository when it is
available. Include the affected version or commit, the impact, reproduction
steps, and any relevant logs or proof of concept. Remove credentials and
personal data before sending a report.

If private advisories are unavailable, contact the repository maintainers
through a private GitHub message and request a security contact.

## Scope

Security reports are especially useful for vulnerabilities involving:

- credential storage, OAuth flows, API-key handling, or secret disclosure;
- capture, sanitization, persistence, or unintended external transmission of
  project and session data;
- project isolation, local daemon authentication, or unsafe integration
  configuration changes;
- backup, restore, or filesystem operations that could expose or overwrite
  user data unexpectedly.

Please allow reasonable time for investigation and remediation before public
disclosure.
