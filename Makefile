SHELL := /bin/bash
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH ?= $(shell dpkg --print-architecture 2>/dev/null || echo amd64)
STAGE := build/deb
DIST := dist
GLIBC := $(shell objdump -T target/release/aether 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1 | sed 's/GLIBC_//')
ifeq ($(GLIBC),)
GLIBC := 2.39
endif

.PHONY: release plugins check check-licenses deb clean

release:
	cargo build --release --workspace

plugins: release
	python3 scripts/stage-plugins.py --target-dir target/release --out build/plugins

check: release
	desktop-file-validate packaging/aether.desktop
	appstreamcli validate --no-net packaging/aether.metainfo.xml

check-licenses:
	python3 scripts/check-license-bundles.py --check-recipes
	@if compgen -G "$(DIST)/aether_*.deb" >/dev/null; then \
		python3 scripts/check-license-bundles.py "$(DIST)"/aether_*.deb; \
	fi

deb: release plugins
	rm -rf "$(STAGE)"
	install -Dm755 target/release/aether "$(STAGE)/usr/bin/aether"
	install -Dm755 target/release/aether-cli "$(STAGE)/usr/bin/aether-cli"
	install -Dm755 target/release/aether-daemon "$(STAGE)/usr/bin/aether-daemon"
	install -Dm755 target/release/aether-native-host "$(STAGE)/usr/bin/aether-native-host"
	install -Dm644 packaging/aether-url-handler.desktop \
		"$(STAGE)/usr/share/applications/aether-url-handler.desktop"
	install -d "$(STAGE)/usr/share/aether/native-messaging"
	install -m644 packaging/native-messaging/*.json \
		"$(STAGE)/usr/share/aether/native-messaging/"
	install -Dm755 scripts/install-native-messaging.sh \
		"$(STAGE)/usr/bin/aether-install-native-messaging"
	install -Dm644 packaging/aether-daemon.service \
		"$(STAGE)/usr/lib/systemd/user/aether-daemon.service"
	install -Dm644 packaging/aether.desktop "$(STAGE)/usr/share/applications/aether.desktop"
	install -Dm644 packaging/aether.metainfo.xml "$(STAGE)/usr/share/metainfo/io.github.pengtang1024.Aether.metainfo.xml"
	for size in 32 48 64 128 256 512; do \
		install -Dm644 "assets/logo-icon-$$size.png" \
			"$(STAGE)/usr/share/icons/hicolor/$${size}x$${size}/apps/aether.png"; \
	done
	install -Dm644 LICENSE "$(STAGE)/usr/share/doc/aether/copyright"
	install -Dm644 THIRD_PARTY_NOTICES.md "$(STAGE)/usr/share/doc/aether/THIRD_PARTY_NOTICES.md"
	install -Dm644 LICENSES/Slint-Royalty-free-2.0.md \
		"$(STAGE)/usr/share/doc/aether/Slint-Royalty-free-2.0.md"
	install -d "$(STAGE)/usr/lib/aether/plugins"
	cp -a build/plugins/. "$(STAGE)/usr/lib/aether/plugins/"
	mkdir -p "$(STAGE)/DEBIAN" "$(DIST)"
	sed -e "s/@VERSION@/$(VERSION)/" \
	    -e "s/@ARCH@/$(ARCH)/" \
	    -e "s/@LIBC@/$(GLIBC)/" \
	    -e "s/@SIZE@/$$(du -sk "$(STAGE)/usr" | cut -f1)/" \
	    packaging/debian/control.in > "$(STAGE)/DEBIAN/control"
	dpkg-deb --build --root-owner-group "$(STAGE)" "$(DIST)/aether_$(VERSION)_$(ARCH).deb"

clean:
	rm -rf build dist
