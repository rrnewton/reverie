#!/usr/bin/env python3
"""Capture existing rust-script inputs without running Cargo or any generated ELF."""
import collections
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parent
H = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917')
CACHE = Path('/tmp/hermit-prejoin-commit-cache-20260917/rust-script')
CONTEXT = H/'ignored/prejoin-main-callers-v8/post-compose-generator-context.json'
LIMIT_BYTES = 512 * 1024**2
read_bytes = 0
records = {}


def read(path):
    global read_bytes
    path = Path(path)
    with path.open('rb') as stream:
        before = os.fstat(stream.fileno())
        assert before.st_size <= 64*1024**2, path
        data = stream.read()
        after = os.fstat(stream.fileno())
    assert (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns), path
    read_bytes += len(data)
    assert read_bytes <= LIMIT_BYTES
    return data


def record(path, role, copy_name=None):
    path = Path(path)
    data = read(path)
    s = path.stat()
    row = {'path': str(path), 'resolved_path': str(path.resolve()), 'bytes': len(data),
           'mode': stat.S_IMODE(s.st_mode), 'sha256': hashlib.sha256(data).hexdigest(),
           'device': s.st_dev, 'inode': s.st_ino, 'mtime_ns': s.st_mtime_ns, 'roles': [role]}
    if path.is_symlink():
        row['symlink_target'] = os.readlink(path)
    prior = records.get(str(path))
    if prior:
        assert (prior['sha256'], prior['bytes'], prior['mode']) == (row['sha256'], row['bytes'], row['mode'])
        prior['roles'] = sorted(set(prior['roles'] + [role]))
        row = prior
    else:
        records[str(path)] = row
    if copy_name:
        dest = ROOT/'copies'/copy_name
        dest.parent.mkdir(parents=True, exist_ok=True)
        with dest.open('xb') as stream:
            stream.write(data)
        row['copy'] = str(dest)
    return row, data


def save(name, value):
    with (ROOT/name).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def main():
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    context_record, data = record(CONTEXT, 'supplied fixed generator context', 'context.json')
    context = json.loads(data)
    manifest_records = context['inputs'][:]
    input_manifests = []
    for item in context['recursive_submodule_inputs']:
        row, data = record(item['path'], 'existing recursive dependency manifest')
        assert row['sha256'] == item['sha256']
        input_manifests.append(row)
        manifest_records.extend(json.loads(data))
    source_ref = context['source_manifest']
    source_record, source_data = record(source_ref['path'], 'existing original tracked-source manifest')
    assert source_record['sha256'] == source_ref['sha256']
    source_manifest = json.loads(source_data)
    bound = {row['path']: row for row in manifest_records}
    source_bound = {str(H/row['path']): row for row in source_manifest}
    all_bound = {**source_bound, **bound}

    registry_roots = sorted(Path('/home/newton/.cargo/registry/src').iterdir())
    represented = collections.Counter()
    for path in bound:
        match = re.match(r'(.*/\.cargo/registry/src/[^/]+/[^/]+)/', path)
        if match:
            represented[match[1]] += 1
    projects = []
    packages = []
    missing_registry = []
    local_sources = []
    for project in sorted((CACHE/'projects').iterdir()):
        assert project.is_dir()
        generated = {}
        for filename in ['Cargo.toml', 'Cargo.lock']:
            row, data = record(project/filename, 'generated rust-script project input', project.name+'/'+filename)
            generated[filename] = row
            if filename == 'Cargo.toml':
                manifest = tomllib.loads(data.decode())
            else:
                lock = tomllib.loads(data.decode())
        bins = []
        for binary in manifest['bin']:
            row, data = record(binary['path'], 'original source of generated rust-script package', project.name+'/original-script.rs')
            expected = source_bound[str(Path(binary['path']))]
            assert row['sha256'] == expected['sha256']
            bins.append({'name': binary['name'], 'source': row, 'covered_by_original_source_manifest': True})
        projects.append({'directory': str(project), 'manifest': generated['Cargo.toml'], 'lock': generated['Cargo.lock'],
                         'name': manifest['package']['name'], 'bins': bins, 'declared_dependencies': manifest.get('dependencies', {}),
                         'package_count': len(lock['package'])})
        for package in lock['package']:
            item = {'project': manifest['package']['name'], **package}
            source = package.get('source', '')
            if source.startswith('registry+'):
                name = package['name']+'-'+package['version']
                dirs = [root/name for root in registry_roots if (root/name).is_dir()]
                assert len(dirs) == 1, (name, dirs)
                directory = dirs[0]
                item['source_directory'] = str(directory)
                item['existing_bound_file_count'] = represented[str(directory)]
                item['cargo_checksum_file_present'] = (directory/'.cargo-checksum.json').exists()
                if not represented[str(directory)]:
                    file_rows = []
                    for base, directories, files in os.walk(directory, followlinks=False):
                        directories.sort()
                        for filename in sorted(files):
                            row, _ = record(Path(base)/filename, 'registry source missing from supplied context: '+name)
                            file_rows.append(row)
                    assert len(file_rows) < 10000
                    archive = Path('/home/newton/.cargo/registry/cache')/directory.parent.name/(name+'.crate')
                    archive_row, archive_bytes = record(archive, 'registry archive checksum for '+name)
                    assert archive_row['sha256'] == package['checksum'], name
                    # Read archive members without extracting anything. The source
                    # records above also bind extraction metadata not in the archive.
                    file_map = {Path(row['path']).relative_to(directory).as_posix(): row for row in file_rows}
                    archive_checked = 0
                    with tarfile.open(archive, 'r:gz') as tar:
                        for member in tar:
                            if not member.isfile():
                                continue
                            relative = Path(member.name).relative_to(name).as_posix()
                            stream = tar.extractfile(member)
                            assert stream is not None
                            expected = hashlib.sha256(stream.read()).hexdigest()
                            assert relative in file_map and file_map[relative]['sha256'] == expected, (name, relative)
                            archive_checked += 1
                    item['missing_source_file_count'] = len(file_rows)
                    item['archive'] = archive_row
                    item['archive_members_match_extracted_files'] = archive_checked
                    missing_registry.append(item)
            elif source.startswith('git+'):
                candidates = [path for path in bound if path.endswith('/'+package['name']+'/Cargo.toml') and '/7d863ab/' in path]
                assert len(candidates) == 1, package
                row, _ = record(candidates[0], 'existing pinned Git package manifest')
                assert row['sha256'] == bound[candidates[0]]['sha256']
                item['existing_manifest_binding'] = row
            else:
                local_sources.append(package['name'])
            packages.append(item)
    assert len(projects) == 2
    assert len(missing_registry) == 23

    # Original local package manifests are already within the supplied source
    # or recursive input manifests. Record the exact resolution without Cargo.
    local_packages = []
    for relative in ['agent-utils/rs/dagrun/Cargo.toml', 'ci/manifest-plan/Cargo.toml', 'detcore-model/Cargo.toml']:
        path = H/relative
        row, data = record(path, 'local package used by generated validate crate')
        assert row['sha256'] == all_bound[str(path)]['sha256']
        local_packages.append({'file': row, 'package': tomllib.loads(data.decode())['package'], 'already_bound': True})

    # Reuse the prior static ELF dependency records only after byte identity.
    prior_inputs = json.loads((ROOT.parent/'inputs-complete.json').read_text())
    prior_map = {row['path']: row for row in prior_inputs}
    prior_elf = {row['path']: row for row in json.loads((ROOT.parent/'elf-dependencies-complete.json').read_text())}
    todo = ['/home/newton/.cargo/bin/rust-script']
    seen = set()
    rust_script_closure = []
    while todo:
        path = todo.pop(0)
        if path in seen:
            continue
        seen.add(path)
        row, _ = record(path, 'rust-script executable or static loader dependency')
        assert row['sha256'] == prior_map[path]['sha256'] and row['bytes'] == prior_map[path]['size']
        rust_script_closure.append(row)
        metadata = prior_elf[row['resolved_path']]
        todo.extend(metadata['interpreters'])
        for dependency in metadata['resolutions']:
            todo.extend(item['path'] for item in dependency['candidates'])

    # Existing target artifacts are observations for the next reuse, never a
    # retrospective claim that the old generator input set included them.
    target = CACHE/'binaries/release'
    generated_commands = []
    for project in projects:
        for binary in project['bins']:
            row, _ = record(target/binary['name'], 'generated rust-script executable available for reuse')
            dep, _ = record(target/(binary['name']+'.d'), 'compiler-emitted source dependency file', project['name']+'/executable.d')
            generated_commands.append({'kind': 'rust-script executable', 'file': row, 'source': binary['source'], 'dependency_file': dep})
    build_outputs = []
    for directory in sorted((target/'build').iterdir()):
        if not directory.is_dir():
            continue
        for path in sorted(directory.iterdir()):
            if path.is_file():
                row, _ = record(path, 'existing Cargo build-script executable or output')
                if path.name == 'build-script-build':
                    generated_commands.append({'kind': 'Cargo build-script executable', 'file': row,
                                               'execution_claim': 'Existing reusable command; presence is not an exact historical invocation record'})
                else:
                    build_outputs.append(row)
    proc_macros = []
    for path in sorted((target/'deps').glob('*.so')):
        row, _ = record(path, 'existing proc-macro shared object used by compiler')
        proc_macros.append(row)

    # Authenticated dependency-file entries connect the source-level script
    # includes to existing input declarations; generated OUT_DIR files can be
    # separately observed in Cargo output rather than treated as tracked source.
    depfile_sources = []
    for project in projects:
        for binary in project['bins']:
            path = target/(binary['name']+'.d')
            text = read(path).decode()
            first = text.splitlines()[0].split(': ', 1)[1]
            for source in first.split():
                src = Path(source)
                if not src.is_absolute():
                    continue
                row, _ = record(src, 'compiler-emitted script dependency input')
                expected = all_bound.get(str(src))
                depfile_sources.append({'path': str(src), 'source_sha256': row['sha256'],
                                        'existing_binding': expected, 'matches_existing': expected is not None and expected['sha256'] == row['sha256']})

    # Preserve the exact producer path and shebang that invoke the new cache.
    producer, _ = record(H/'ci/manifest-plan/src/validation_dag.rs', 'actual generated_plan script invocation source', 'validation_dag.rs')
    assert producer['sha256'] == source_bound[str(H/'ci/manifest-plan/src/validation_dag.rs')]['sha256']
    save('CACHE-PROJECTS.json', projects)
    save('LOCK-PACKAGES.json', packages)
    save('MISSING-REGISTRY-PACKAGES.json', missing_registry)
    save('LOCAL-PACKAGES.json', local_packages)
    save('RUST-SCRIPT-LOADER.json', rust_script_closure)
    save('GENERATED-COMMANDS.json', generated_commands)
    save('GENERATED-BUILD-OUTPUTS.json', build_outputs)
    save('GENERATED-PROC-MACROS.json', proc_macros)
    save('DEPFILE-SOURCE-COVERAGE.json', depfile_sources)
    save('ADDITIONAL-INPUTS.json', [row for row in sorted(records.values(), key=lambda row: row['path'])
                                  if row['path'] not in all_bound and 'supplied fixed generator context' not in row['roles']])
    save('OBSERVED-FILES.json', sorted(records.values(), key=lambda row: row['path']))
    summary = {'started_at': started, 'ended_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
               'context': context_record, 'source_manifest': source_record, 'dependency_manifests': input_manifests,
               'context_source_identity': context['scm'], 'projects': len(projects), 'package_rows': len(packages),
               'validate_registry_packages': 122, 'missing_registry_versions': len(missing_registry),
               'missing_registry_files': sum(x['missing_source_file_count'] for x in missing_registry),
               'rust_script_loader_files': len(rust_script_closure), 'generated_commands': len(generated_commands),
               'proc_macro_objects': len(proc_macros), 'depfile_sources': len(depfile_sources),
               'depfile_sources_not_matching_existing': [x for x in depfile_sources if not x['matches_existing']],
               'read_bytes': read_bytes, 'read_limit_bytes': LIMIT_BYTES,
               'scope': 'Read-only static cache/source/input comparison. Owner reported generation terminal before final capture. No process/ELF execution, Cargo, test, guest, network, cache/source/ref change.'}
    save('CAPTURE.json', summary)
    print(json.dumps({key: summary[key] for key in ['projects','package_rows','missing_registry_versions','missing_registry_files',
                                                 'rust_script_loader_files','generated_commands','proc_macro_objects','depfile_sources',
                                                 'read_bytes','depfile_sources_not_matching_existing']}))


if __name__ == '__main__':
    main()
