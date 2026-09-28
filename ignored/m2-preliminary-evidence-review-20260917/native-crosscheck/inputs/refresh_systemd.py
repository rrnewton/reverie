"""Freeze a fresh exact command line for the already identified systemd process."""
import os
from pathlib import Path
import re
from common import HERE, REPO, bounded_process, check_file, file_record, read, require, write_new


def refresh_systemd(context, prefix):
    binding = context['non_build_processes']
    require(read('/proc/sys/kernel/random/boot_id',128).decode().strip() == binding['boot_id'], 'different boot')
    rows = [row for row in binding['processes'] if row['pid'] == 32320]
    require(len(rows) == 1 and len(binding['processes']) == 3, 'different protected process population')
    old = rows[0]
    require(old['parent'] == 1 and old['start_ticks'] == '5521' and old['cgroup'] == '0::/user.slice/user-212630.slice/user@212630.service/init.scope\n', 'different systemd generation or cgroup')
    require(re.fullmatch(r'/usr/lib/systemd/systemd --user --deserialize=[0-9]+', old['argv']) is not None, 'different previous argument structure')
    executable = next(row for row in context['inputs'] if row['path'] == '/usr/lib/systemd/systemd')
    check_file(executable, executable=True)
    proc = Path('/proc/32320')

    def identity():
        require(proc.stat().st_uid == 212630 == os.getuid(), 'different systemd UID')
        raw = read(proc/'stat',65536).decode()
        fields = raw[raw.rindex(')')+2:].split()
        require(int(fields[1]) == old['parent'] and fields[19] == old['start_ticks'], 'systemd process generation changed')
        require(read(proc/'cgroup',4096).decode() == old['cgroup'], 'systemd cgroup changed')
        return raw

    before = identity()
    cmdline = read(proc/'cmdline',4096)
    require(cmdline.endswith(b'\0'), 'incomplete systemd argv')
    argv = cmdline[:-1].decode().split('\0')
    require(len(argv) == 3 and argv[:2] == ['/usr/lib/systemd/systemd','--user'] and re.fullmatch(r'--deserialize=[0-9]+',argv[2]) is not None, 'unexpected systemd executable/arguments')
    query = bounded_process(['/bin/ps','-p','32320','-o','pid=,ppid=,args='], REPO, context['environment'], Path(str(prefix)+'-ps'), 5, 8192)
    require(query['returncode'] == 0 and not query['forced'], 'systemd observation failed')
    parts = read(query['stdout']['path'],8192).decode().strip().split(None,2)
    require(parts == ['32320','1',' '.join(argv)], 'systemd changed between cmdline and ps observations')
    after = identity()
    require(read(proc/'cmdline',4096) == cmdline, 'systemd command line changed during observation')
    # The known protected process permits cmdline/stat/cgroup reads but not exe.
    # Preserve that distinction; the on-disk binary was independently bound above.
    try:
        proc_exe = dict(path=os.readlink(proc/'exe'))
        require(proc_exe['path'] == '/usr/lib/systemd/systemd', 'different proc executable')
    except PermissionError:
        proc_exe = dict(permission_denied=True, observed_inode=False)
    actual = dict(old, argv=' '.join(argv))
    record = Path(str(prefix)+'.json')
    write_new(record, dict(boot_id=binding['boot_id'], previous=old, actual=actual, uid=212630,
                           raw_cmdline_hex=cmdline.hex(), argv=argv, stat_before=before, stat_after=after,
                           ps=query, executable_file=executable, proc_exe=proc_exe,
                           meaning='Only the decimal deserialize argument may change during preparation. The runtime census still requires the complete newly frozen identity. No other process or descendant is exempted.'))
    context['non_build_processes']['processes'] = [actual if row['pid']==32320 else row for row in binding['processes']]
    context['inputs'] += [file_record(record), file_record(Path(__file__).resolve())]
