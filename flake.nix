{
  description = "pizza-delivery: autonomous prompt queue that ends at a draft PR";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      home-manager,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      nodeModulesHash = "sha256-OoMYfyfPJm3XpxyEKja/mCUfyZENUx0aTe5e/90MAvs=";
      build =
        pkgs:
        pkgs.callPackage ./nix/package.nix {
          craneLib = crane.mkLib pkgs;
          inherit nodeModulesHash;
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        pizza-delivery = build pkgs;
        default = pizza-delivery;
      });

      overlays.default = final: _prev: { pizza-delivery = build final; };

      homeManagerModules.default = import ./nix/hm-module.nix self;

      checks = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          home = home-manager.lib.homeManagerConfiguration {
            inherit pkgs;
            modules = [
              self.homeManagerModules.default
              {
                home = {
                  username = "tester";
                  homeDirectory = "/home/tester";
                  stateVersion = "24.11";
                };
                services.pizza-delivery = {
                  enable = true;
                  settings.routing.default = "claude";
                };
              }
            ];
          };
        in
        {
          package = self.packages.${system}.default;
          module = pkgs.runCommand "pizza-delivery-module-check" { } ''
            files=${home.config.home-files}/.config
            test -f $files/systemd/user/pizza-delivery.socket
            test -f $files/systemd/user/pizza-delivery.service
            grep -q 'ListenStream=%t/pizza-delivery.sock' $files/systemd/user/pizza-delivery.socket
            grep -q 'daemon' $files/systemd/user/pizza-delivery.service
            grep -q 'claude' $files/pizza-delivery/routing.toml
            touch $out
          '';
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            bun
          ];
        };
      });
    };
}
