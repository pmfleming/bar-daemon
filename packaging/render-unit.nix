# Only used for packaged defaults; module integrations use native unit generators.
lib: unit:
lib.concatStringsSep "\n" (
  lib.mapAttrsToList (
    section: values:
    "[${section}]\n"
    + lib.concatStringsSep "\n" (
      lib.mapAttrsToList (
        key: value:
        "${key}=" + (if builtins.isList value then lib.concatStringsSep " " value else toString value)
      ) values
    )
    + "\n"
  ) unit
)
