bigos — barely implemented general operating system

[bootimage]: https://crates.io/crates/bootimage

# Required tools

You have to install the following tools yourself:

- qemu
- llvm tools
- [`bootimage`][bootimage] tool: `cargo install bootimage`

If you are on Debian system, install the required packages with:

```sh
sudo apt-get install -y qemu-system-x86 llvm-14-tools
```

## Optional tools

- [lefthook](https://github.com/evilmartians/lefthook): for pre-commit and pre-push hooks.

To install the hooks:

- install lefthook
- run `lefthook install` in the project root

If you want to skip hooks, use `git commit --no-verify` or `git push --no-verify`

# Running

1. Using [`bootimage runner`][bootimage]:

- build and run the kernel: `cargo run`

2. Running qemu explicitly:

- build the kernel: `cargo build`
- run qemu: `qemu-system-x86_64 -drive format=raw,file=target/x86_64-bigos/debug/bootimage-bigos.bin`
