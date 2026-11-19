{
  description = "iptop: a modern, btop-flavoured network bandwidth monitor";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      # AF_PACKET capture is Linux-only.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "armv7l-linux"
      ];
      forSystems = ss: f: lib.genAttrs ss (system: f nixpkgs.legacyPackages.${system});
      forAllSystems = forSystems systems;
    in
    {
      packages = forAllSystems (pkgs: rec {
        iptop = pkgs.callPackage ./nix/package.nix { };
        default = iptop;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.iptop;
          meta.description = "Run iptop";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.iptop ];
          packages = with pkgs; [
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });

      # Built with the consumer's package set, so cross-compilation works.
      overlays.default = final: _prev: {
        iptop = final.callPackage ./nix/package.nix { };
      };

      # `programs.iptop.enable = true;` installs a capability wrapper at
      # /run/wrappers/bin/iptop so it runs without sudo.
      nixosModules.default =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.iptop;
        in
        {
          options.programs.iptop = {
            enable = lib.mkEnableOption "iptop with cap_net_raw/cap_net_admin";
            package = lib.mkOption {
              type = lib.types.package;
              default = pkgs.callPackage ./nix/package.nix { };
              defaultText = lib.literalMD "iptop built from this flake";
              description = "The iptop package to wrap.";
            };
          };
          config = lib.mkIf cfg.enable {
            security.wrappers.iptop = {
              owner = "root";
              group = "root";
              capabilities = "cap_net_raw,cap_net_admin+p";
              source = lib.getExe cfg.package;
            };
          };
        };

      # nixfmt needs GHC, which cannot bootstrap on armv7l.
      formatter = forSystems [ "x86_64-linux" "aarch64-linux" ] (pkgs: pkgs.nixfmt);
    };
}
