PREFIX ?= /usr/local

build:
	cargo build --release

install: build
	cp target/release/zestful $(PREFIX)/bin/zestful

uninstall:
	rm -f $(PREFIX)/bin/zestful

.PHONY: build install uninstall
