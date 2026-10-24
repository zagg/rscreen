# rscreen — terminal multiplexer en Rust
BIN     := rscreen
PREFIX  ?= /usr/local
TARGET  := target/release/$(BIN)

.PHONY: all build release debug test itest itest-cli itest-rename dump install uninstall clean fmt clippy

all: release

build debug:
	cargo build

release:
	cargo build --release

## tests unitaires
test:
	cargo test

## bout-en-bout : rscreen piloté dans un vrai pty (49 vérifs, 53 avec pyte),
## le ciblage des sessions depuis la CLI, puis le banc d'essai « r »
## (renommage de session dans la modale).
## Avec pyte installé, les assertions portent sur l'écran réellement affiché.
itest: release itest-cli
	RSCREEN=$(TARGET) python3 tests/drive.py
	RSCREEN=$(TARGET) python3 tests/rename.py

## ciblage de session en CLI : nom, pid, `pid.machin`, `-r` seul (15 vérifs)
itest-cli: release
	RSCREEN=$(TARGET) python3 tests/cli.py

## renommage d'une session depuis la fenêtre modale (23 vérifs)
itest-rename: release
	RSCREEN=$(TARGET) python3 tests/rename.py

## rendu "visuel" : l'écran du client est rejoué dans un émulateur VT (pyte)
dump: release
	python3 tests/screen_dump.py

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
