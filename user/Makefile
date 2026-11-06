CC = clang
LD = ld.lld
AR = llvm-ar

TARGET = x86_64-unknown-elf

CFLAGS = --target=$(TARGET) \
         -ffreestanding \
         -nostdlib \
         -nostdinc \
         -fno-builtin \
         -fno-stack-protector \
         -mno-red-zone \
         -m64 \
         -Wall \
         -Wextra \
         -I. \
         -g

LDFLAGS = -T linker.ld \
          -nostdlib \
          -static

.PHONY: all rust-programs clean

all: $(PROGRAMS) rust-programs

rust-programs:
	cd rustspace && cargo fmt && cargo +nightly build -Z build-std=core,alloc -Z json-target-spec --target x86_64-user.json

clean:
	cd rustspace && cargo clean
