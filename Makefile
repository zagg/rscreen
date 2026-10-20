# rscreen — terminal multiplexer en Rust
BIN     := rscreen
PREFIX  ?= /usr/local
TARGET  := target/release/$(BIN)

.PHONY: all build release debug test itest dump install uninstall clean fmt clippy

all: release

build debug:
	cargo build

release:
	cargo build --release

## tests unitaires
test:
	cargo test

## bout-en-bout : rscreen piloté dans un vrai pty (38 vérifications)
itest: release
	python3 tests/drive.py

## rendu "visuel" : l'écran du client est rejoué dans un émulateur VT (pyte)
dump: release
	/tmp/vt/bin/python tests/screen_dump.py || python3 tests/screen_dump.py

install: release
	install -Dm755 $(TARGET) $(DESTDIR)$(PREFIX)/bin/$(BIN)

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BIN)

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets

clean:
	cargo clean
