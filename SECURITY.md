# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Use GitHub's private reporting:
[Report a vulnerability](https://github.com/giacomomicoli/open-surround/security/advisories/new)
(Security tab → "Report a vulnerability"). Only the maintainer sees the report. You will get an
answer as soon as possible, and the fix and advisory will be coordinated with you.

Include what you can: the affected version (`os-ctl versione`), what an attacker needs (local
user, network position, a crafted file), the steps to reproduce and the impact.

## Supported versions

Only the latest release gets security fixes. Installations from a release update from the
settings window, which installs only packages signed with the release key.

## Scope

Particularly relevant:

- **Updates** (`os-update`, the window's update banner): anything that makes an installation
  accept a package that is not signed with the key in `dist/linux/minisign.pub`, write outside
  the installation paths (`~/.local/bin`, `~/.local/share/open-surround`,
  `~/.config/systemd/user`, `~/.local/share/applications`), or skip the checks on the DLL.
- **Release pipeline** (`.github/workflows/`): anything that would let someone other than the
  maintainer publish a signed package, or leak the signing key.
- **Control socket** (`$XDG_RUNTIME_DIR/open-surround/control.sock`): commands reaching the node
  from other users, or input that crashes it.
- **PE loader** (`os-pe-loader`): it runs code from a DLL the user supplies, which is trusted by
  design. Issues where a crafted DLL escapes what the loader is meant to do are still welcome,
  but the DLL itself is not a security boundary.

Out of scope: Dolby's module itself, and problems that need the attacker to already run code as
the same user.

## Release signing

Release archives are signed with minisign (Ed25519). The public key is
[`dist/linux/minisign.pub`](dist/linux/minisign.pub) and is compiled into `os-update`. To verify
a download by hand:

```
minisign -Vm open-surround-<version>-linux-x86_64.tar.gz -p dist/linux/minisign.pub
```

Release tags (`v*`) cannot be moved or deleted once pushed.
