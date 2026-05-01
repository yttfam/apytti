BINARY = apytti
# Bundle ID has .app suffix per Apple TCC rules: bumping the bundle ID is the
# only way to reset a previously-denied Local Network Privacy decision. Pkg
# receipt ID stays plain so installer history is preserved.
IDENTIFIER = net.calii.apytti.app
PKG_IDENTIFIER = net.calii.apytti
VERSION = $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP_SIGN = Developer ID Application: Nico Bousquet (XJQQCN392F)
PKG_SIGN = Developer ID Installer: Nico Bousquet (XJQQCN392F)
NOTARY_PROFILE = APYTTI_NOTARY
ENTITLEMENTS = entitlements.plist
PKG_ROOT = target/pkg-root
PKG = target/$(BINARY)-$(VERSION).pkg
APP_BUNDLE = target/Apytti.app

.PHONY: build release sign bundle pkg dist clean

build:
	cargo build

release:
	cargo build --release

sign: release
	codesign --force --options runtime --timestamp \
		--sign "$(APP_SIGN)" \
		--identifier "$(IDENTIFIER)" \
		--entitlements $(ENTITLEMENTS) \
		target/release/$(BINARY)
	codesign -dvvv target/release/$(BINARY)

# Wrap the CLI in a real .app bundle. The Info.plist embedded here is what
# TCC keys Local Network grants against on SIP-enabled macOS; the bare binary
# also gets one inlined via build.rs for cases where it's run outside the bundle.
bundle: release
	rm -rf $(APP_BUNDLE)
	mkdir -p $(APP_BUNDLE)/Contents/MacOS $(APP_BUNDLE)/Contents/Resources
	cp target/release/$(BINARY) $(APP_BUNDLE)/Contents/MacOS/$(BINARY)
	chmod 755 $(APP_BUNDLE)/Contents/MacOS/$(BINARY)
	sed "s/__VERSION__/$(VERSION)/g" bundle/Info.plist.template > $(APP_BUNDLE)/Contents/Info.plist
	codesign --force --options runtime --timestamp \
		--sign "$(APP_SIGN)" \
		--identifier "$(IDENTIFIER)" \
		--entitlements $(ENTITLEMENTS) \
		$(APP_BUNDLE)/Contents/MacOS/$(BINARY)
	codesign --force --options runtime --timestamp \
		--sign "$(APP_SIGN)" \
		--identifier "$(IDENTIFIER)" \
		--entitlements $(ENTITLEMENTS) \
		$(APP_BUNDLE)
	codesign -dvvv $(APP_BUNDLE)

# Signed + notarized + stapled .pkg installing Apytti.app to /Applications
# plus a /usr/local/bin/apytti symlink for shell users.
pkg: bundle
	rm -rf $(PKG_ROOT)
	mkdir -p $(PKG_ROOT)/Applications $(PKG_ROOT)/usr/local/bin
	cp -R $(APP_BUNDLE) $(PKG_ROOT)/Applications/Apytti.app
	ln -sf /Applications/Apytti.app/Contents/MacOS/$(BINARY) $(PKG_ROOT)/usr/local/bin/$(BINARY)
	pkgbuild --root $(PKG_ROOT) \
		--identifier $(PKG_IDENTIFIER) \
		--version $(VERSION) \
		--install-location / \
		--scripts bundle/scripts \
		--sign "$(PKG_SIGN)" \
		$(PKG)
	xcrun notarytool submit $(PKG) \
		--keychain-profile $(NOTARY_PROFILE) --wait
	xcrun stapler staple $(PKG)
	xcrun stapler validate $(PKG)

dist: pkg
	@echo "Distribution pkg: $(PKG)"
	@shasum -a 256 $(PKG)

linux:
	cargo build --release --target x86_64-unknown-linux-musl

clean:
	cargo clean
	rm -rf $(PKG_ROOT) $(APP_BUNDLE) target/*.pkg
