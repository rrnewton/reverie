#!/usr/bin/python3
"""Append static interpreter and command-lookup inputs without executing guests."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil

import bind_inputs as binding

ROOT = binding.ROOT


def save(name, value):
    with (ROOT / name).open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def observe(path, role):
    path = Path(path)
    result = {"path": str(path), "exists": path.exists(), "lexists": os.path.lexists(path)}
    if path.exists():
        row = binding.bind(path, role)
        result.update({"resolved_path": row["resolved_path"], "sha256": row["sha256"]})
    return result


def main():
    binding.records.update({x["path"]: x for x in json.loads((ROOT / "inputs.json").read_text())})
    binding.absent.extend(json.loads((ROOT / "unavailable.json").read_text()))
    binding.elf_records.extend(json.loads((ROOT / "elf-dependencies.json").read_text()))
    binding.inspected.update(x["path"] for x in binding.elf_records)

    dry_runs = sorted((ROOT / "queries").glob("*-driver-dry-run/stderr"))
    plugins = set()
    for path in dry_runs:
        plugins.update(re.findall(r"(?:^|\s)-plugin\s+(\S+)", path.read_text()))
    assert plugins == {"/usr/libexec/gcc/x86_64-redhat-linux/11/liblto_plugin.so"}, plugins
    for plugin in plugins:
        binding.bind(plugin, "actual GCC -### collect2 -plugin argument")
    binding.bind("/usr/lib64/libpthread.a", "-lpthread archive found on the dry-run default search path")

    # These six directories are compiled into the bound libperl and are also
    # declared by its Config files. We inspect every candidate; this is not a
    # runtime trace or a claim that all conditional modules will be loaded.
    module_roots = [
        "/usr/local/lib64/perl5/5.32", "/usr/local/share/perl5/5.32",
        "/usr/lib64/perl5/vendor_perl", "/usr/share/perl5/vendor_perl",
        "/usr/lib64/perl5", "/usr/share/perl5",
    ]
    modules = [
        "Fcntl.pm", "POSIX.pm", "strict.pm", "warnings.pm", "XSLoader.pm",
        "Exporter.pm", "Exporter/Heavy.pm", "Carp.pm", "Carp/Heavy.pm",
        "DynaLoader.pm", "Config.pm", "Config_heavy.pl", "Config_git.pl",
        "Tie/Hash.pm", "warnings/register.pm", "overloading.pm", "overload.pm",
        "overload/numbers.pm", "Scalar/Util.pm", "List/Util.pm", "mro.pm",
    ]
    module_records = []
    source_edges = []
    for module in modules:
        candidates = []
        for directory in module_roots:
            candidate = Path(directory) / module
            candidates.append(observe(candidate, "Perl direct or conditional source dependency " + module))
            if module.endswith(".pm"):
                candidates.append(observe(str(candidate) + "c", "Perl compiled-module precedence candidate " + module))
        assert any(row["exists"] for row in candidates), module
        module_records.append({"module": module, "candidates": candidates})
        for item in candidates:
            if not item["exists"] or item["path"].endswith(".pmc"):
                continue
            in_pod = False
            edges = []
            for number, line in enumerate(Path(item["path"]).read_text().splitlines(), 1):
                if line in ["__END__", "__DATA__"]:
                    break
                if line.startswith("=cut"):
                    in_pod = False
                    continue
                if re.match(r"^=[a-zA-Z]", line):
                    in_pod = True
                if not in_pod and not line.lstrip().startswith("#") and re.search(r"\b(?:use|require|XSLoader::load|bootstrap)\b", line):
                    edges.append({"line": number, "text": line})
            source_edges.append({"path": item["path"], "source_references": edges})

    extensions = []
    for module in ["Fcntl", "POSIX", "mro", "List/Util", "Scalar/Util"]:
        stem = module.rsplit("/", 1)[-1]
        rows = []
        for directory in module_roots:
            for suffix in [".so", ".bs"]:
                rows.append(observe(Path(directory) / "auto" / module / (stem + suffix),
                                    "Perl XS or optional bootstrap candidate " + module))
        if module != "Scalar/Util":
            assert any(x["exists"] and x["path"].endswith(".so") for x in rows), module
        extensions.append({"module": module, "candidates": rows})

    sitecustomize = [observe(Path(root) / "sitecustomize.pl", "conditional Perl startup customization")
                     for root in module_roots]
    libperl = Path("/lib64/libperl.so.5.32").read_bytes()
    static_strings = sorted({s.decode("ascii") for s in libperl.split(b"\0")
                             if (s.startswith(b"/") and b"/perl" in s and len(s) < 500)
                             or (b"sitecustomize" in s and len(s) < 500)})
    save("perl-inputs.json", {
        "discovery": "Static Config/source/libperl inspection only; no Perl or loader execution",
        "module_roots": module_roots, "module_candidates": module_records,
        "extensions": extensions, "sitecustomize": sitecustomize,
        "source_references": source_edges, "static_libperl_strings": static_strings,
        "scope": "Direct Fcntl/POSIX dependencies and their source-visible conditional loader/error dependencies; not an observed runtime-open set",
    })

    command_records = []
    commands = ["cc", "as", "ld", "timeout", "rust-script", "cargo-nextest"]
    for command in commands:
        actual = shutil.which(command, path=binding.ENV["PATH"])
        assert actual, command
        attempts = []
        for directory in binding.ENV["PATH"].split(":"):
            path = Path(directory) / command
            record = observe(path, "PATH candidate for " + command)
            record["executable"] = os.access(path, os.X_OK) and path.is_file()
            attempts.append(record)
            if record["executable"]:
                assert str(path) == actual
                break
        command_records.append({"command": command, "selected": actual, "attempts_through_selected": attempts})
    assert command_records[0]["selected"] == "/usr/bin/cc"
    protected = ["GCC_EXEC_PREFIX", "COMPILER_PATH", "LIBRARY_PATH", "CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH",
                 "LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT", "CC", "CFLAGS", "CPPFLAGS", "CXXFLAGS", "LDFLAGS",
                 "PERL5LIB", "PERLLIB", "PERL5OPT", "PERL_LOCAL_LIB_ROOT", "PERL_USE_UNSAFE_INC", "BASH_ENV", "ENV"]
    assert not any(name in binding.ENV for name in protected)
    save("lookup-inputs.json", {"PATH": binding.ENV["PATH"], "commands": command_records,
                               "environment_keys_absent": protected,
                               "environment_sha256": hashlib.sha256((ROOT / "environment.json").read_bytes()).hexdigest()})

    binding.process_elf_queue()
    save("inputs-complete.json", sorted(binding.records.values(), key=lambda r: r["path"]))
    save("elf-dependencies-complete.json", binding.elf_records)
    save("unavailable-complete.json", {
        "original_observations": binding.absent,
        "dispositions": {
            "liblto_plugin.so": "The bare-name GCC query was unresolved, but all five dry runs name the existing bound absolute plugin; this required input is available.",
            "libpthread.so": "The optional .so name is unresolved. The -pthread dry run uses -lpthread; the existing default-search libpthread.a is bound, as is integrated libc.",
            "specs": "No external specs file was found. GCC reports built-in specs; the complete -dumpspecs output is retained.",
            "/etc/ld.so.preload": "Absent; preserve absence during admission instead of treating this as a required file.",
        },
        "required_unavailable": [x for x in binding.absent if x["required"]],
    })
    print(json.dumps({"files": len(binding.records), "unique_elf_files": len(binding.elf_records),
                      "modules": len(modules), "required_unavailable": [x for x in binding.absent if x["required"]]}))


if __name__ == "__main__":
    main()
