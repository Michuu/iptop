{ lib, rustPlatform }:

let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  # The build sandbox's passwd has no real home for root.
  checkFlags = [ "--skip=util::tests::home_lookup" ];

  meta = {
    description = cargoToml.package.description;
    license = lib.licenses.gpl2Plus;
    mainProgram = "iptop";
    platforms = lib.platforms.linux;
  };
}
