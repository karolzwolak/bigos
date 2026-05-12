# BigOS

BigOS — barely implemented general operating system

## Development / running

### Prerequisites

- QEMU

- llvm tools

- rustc 1.96.0-nightly

### Instructions

Install the required packages for your distro:

| Distro          | Command                                              |
| --------------- | ---------------------------------------------------- |
| Debian / Ubuntu | `sudo apt install qemu-system-x86 llvm ovmf`         |
| Arch Linux      | `sudo pacman -S qemu-system-x86 llvm edk2-ovmf`      |
| Fedora / RHEL   | `sudo dnf install qemu-system-x86 llvm edk2-ovmf`    |
| openSUSE        | `sudo zypper install qemu-x86 llvm qemu-ovmf-x86_64` |

This project uses `cargo xtask` (or shorter `cargo x`) for development tasks.
Run `cargo xtask --help` to see the available commands.

## Optional tools

- [lefthook](https://github.com/evilmartians/lefthook): for pre-commit and pre-push hooks.

To install the hooks:

- install lefthook
- run `lefthook install` in the project root

If you want to skip hooks, use `git commit --no-verify` or `git push --no-verify`
