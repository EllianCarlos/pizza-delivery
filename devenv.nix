{ pkgs, ... }:

{
  languages.rust.enable = true;

  languages.javascript = {
    enable = true;
    bun.enable = true;
  };

  packages = [ pkgs.git ];
}
