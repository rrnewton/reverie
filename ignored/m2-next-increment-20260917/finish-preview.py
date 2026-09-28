from pathlib import Path
import re
D=Path(__file__).parent/'preview'
p=D/'hermit-cli/src/bin/hermit/run.rs';s=p.read_text();a=s.index('    /// Compare the internal logs under the CANONICAL parity policy:');b=s.index('    #[clap(long, requires = "verify")]\n    verify_strict',a);s=s[:a]+'''    /// Compatibility spelling for the default canonical INFO comparison.
    /// Every --verify compares exit status, stdout and stderr exactly, and all
    /// INFO records after removing real wall-clock prefixes and ordinalizing
    /// explicitly marked host addresses. Numeric values remain exact.
    /// Use --verify-verbose for an all-level diagnostic comparison.
'''+s[b:]
a=s.index('    /// Which comparator a `--verify` run uses');b=s.index('    fn verification_strictness',a);s=s[:a]+'''    /// The default policy is independent of backend and compatibility flags.
    #[cfg(test)]
'''+s[b:]
s=s.replace('"plain --verify on {value} must stay on the lossy comparator, so it \\\n             cannot claim canonical bitwise parity"','"plain --verify on {value} must use canonical INFO comparison"')
# This import is now used only by the all-backend unit controls.
s=s.replace('use super::verify::LogCompareStrictness;','#[cfg(test)]\nuse super::verify::LogCompareStrictness;')
p.write_text(s)
p=D/'hermit-cli/src/bin/hermit/record_start.rs';s=p.read_text();end=s.index('    #[clap(long',s.index('    verify_json:')) if False else -1
# Replace the documentation immediately associated with verify_strict without changing parsing.
idx=s.index('    verify_strict: bool,');a=s.rfind('\n    ///',0,s.rfind('    #[clap',0,idx));
while a>0 and s[s.rfind('\n',0,a)+1:a].strip().startswith('///'): a=s.rfind('\n',0,a)
# Use contiguous doc-comment block preceding the attribute.
lines=s.splitlines(True);i=next(i for i,l in enumerate(lines) if '    verify_strict: bool,' in l);j=i-1
while j>=0 and (lines[j].lstrip().startswith(('///','#[clap')) or not lines[j].strip()):j-=1
attrs=''.join(l for l in lines[j+1:i] if l.lstrip().startswith('#[clap'))
lines[j+1:i]=['    /// Compatibility spelling for the default canonical INFO comparison.\n','    /// Recording and replay still report their actual time-virtualization policy.\n',attrs];p.write_text(''.join(lines))
p=D/'hermit-cli/src/bin/hermit/logdiff.rs';s=p.read_text();s=s.replace('    /// Compare the canonical INFO stream used by --verify-strict within the\n    /// selected record envelope. With --json this comparison is mandatory and\n    /// selected automatically.','    /// Compatibility spelling for the default canonical INFO comparison.\n    /// The selected record envelope also applies without this flag or --json.');p.write_text(s)
p=D/'hermit-cli/src/bin/hermit/verify.rs';s=p.read_text();a=s.index('    // The core of the strip-lines/verdict decoupling:');b=s.index('    #[test]',a);s=s[:a]+'''    // Hold guest outputs/status constant: default verification must detect a
    // difference in the actual numeric INFO payload and retain both logs.
'''+s[b:]
s=s.replace('    /// Build the spec (and, implicitly, the concrete diff flags) from the\n    /// requested strictness and whether logs are compared at all. This is the\n    /// single place the strictness label maps onto `strip_lines`/`full_trace`,\n    /// so the flags the diff engine sees and the flags the verdict reports can\n    /// never drift apart.','    /// Build the canonical policy and its report from the observation scope.\n    /// Historical stripped report values remain readable but cannot select an\n    /// active comparison here.')
s=s.replace('    // Resolve the strictness label to concrete diff flags once, and carry the\n    // resulting spec through to the verdict so the returned outcome records\n    // exactly which comparison certified it.','    // Construct the canonical policy once and carry it through to the verdict.')
s=s.replace('            // The comparison semantics come from `spec` (strip_lines + mode); only','            // The comparison semantics come from `spec`; only')
p.write_text(s)
p=D/'detcore/src/logdiff.rs';s=p.read_text().replace('    use clap::CommandFactory;\n','')
# Clear descriptions of removed functionality, retaining historical reasoning only where explicit.
s=re.sub(r'/// A scheduler COMMIT for.*?\n(?=fn |/// |#\[)',lambda m:m.group(0),s,flags=re.S) if False else s
for name,indices in [('test_info_selection',[0,1,2]),('test_info_selection_preserves_all_classes',[0,1,2]),('test_info_selection_preserves_io_polling_bookkeeping',[0,2,3]),('test_info_selection_preserves_sabre_internal_pipe_resource_turn',[0,1]),('test_info_selection_preserves_sabre_loopback_poll_yield',[0,1])]:
 a=s.index('    fn '+name+'(');e=s.index('\n    }',a);b=s[a:e];b=b.replace('indexed_text(&input.iter().filter(|message| message.text.starts_with("INFO ")).copied().collect::<Vec<_>>())','indexed_text(&['+', '.join('input['+str(i)+']' for i in indices)+'])');s=s[:a]+b+s[e:]
# Existing explanatory blocks now describe the intended stricter contract.
a=s.index('    /// Regression: the deterministic comparison must ignore');b=s.index('    #[test]',a);s=s[:a]+'''    /// INFO scheduler bookkeeping remains compared; only DEBUG diagnostics are
    /// outside the default observation scope.
'''+s[b:]
a=s.index('    /// Regression: two runs that differ only');b=s.index('    #[test]',a);s=s[:a]+'''    /// An extra INFO poll-retry turn is a difference even if guest syscalls agree.
'''+s[b:]
for marker,doc in [('    /// Erasing a `/tmp` path','    /// Preserve the exact fields after a temporary path.\n'),('    /// The narrowed pattern','    /// Different temporary paths must remain distinguishable.\n'),('    /// Two distinct `/tmp` paths','    /// Every temporary path on one record remains exact.\n'),('    /// Every other test here forces','    /// The default INFO scope compares kick asymmetry and preserves diagnostic\n    /// counts on a matching pair.\n'),('    /// The reason both sides must be printed','    /// Different committed virtual times must diverge under the default policy.\n')]:
 a=s.index(marker);b=s.index('    #[test]',a);s=s[:a]+doc+s[b:]
# A pre-existing negative control needs explicit exact-address mode now that default is canonical.
a=s.index('        // Positive control: raw comparison');b=s.index('\n    }',a);part=s[a:b];part=part.replace('comparison: super::LogComparisonMode::FullTrace,','comparison: super::LogComparisonMode::FullTrace,\n            canonicalize_addresses: false,');s=s[:a]+part+s[b:]
p.write_text(s)
