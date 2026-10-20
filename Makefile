# rscreen — terminal multiplexer en Rust
BIN     := rscreen
PREFIX  ?= /usr/local
TARGET  := target/release/$(BIN)

.PHONY: all build release debug test itest itest-rename dump install uninstall clean fmt clippy

all: release

build debug:
	cargo build

release:
	cargo build --release

## tests unitaires
test:
	cargo test

## bout-en-bout : rscreen piloté dans un vrai pty (49 vérifs, 53 avec pyte),
## suivi du banc d'essai « r » (renommage de session dans la modale).
## Avec pyte installé, les assertions portent sur l'écran réellement affiché.
itest: release itest-rename
	RSCREEN=$(TARGET) python3 tests/drive.py

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
