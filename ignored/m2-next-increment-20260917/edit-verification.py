from pathlib import Path
import re
D=Path(__file__).parent/'preview'
def function(s,name,replacement,indent=''):
 m=re.search(r'^'+re.escape(indent)+r'(?:pub(?:\([^)]*\))? )?fn '+re.escape(name)+r'\b',s,re.M);assert m,name;e=s.index('\n'+indent+'}',m.start())+len(indent)+2;return s[:m.start()]+replacement+s[e:]
def remove_arg(s,name,n):
 matches=list(re.finditer(r'\b'+re.escape(name)+r'\s*\(',s))
 for m in reversed(matches):
  if re.search(r'fn\s+$',s[max(0,m.start()-5):m.start()]):continue
  i=m.end();start=i;depth=0;quoted=False;escape=False;args=[]
  while i<len(s):
   c=s[i]
   if quoted:
    if escape:escape=False
    elif c=='\\':escape=True
    elif c=='"':quoted=False
   elif c=='"':quoted=True
   elif c in '([{':depth+=1
   elif c in ')]}':
    if c==')' and depth==0:
     if s[start:i].strip():args.append((start,i))
     break
    depth-=1
   elif c==',' and depth==0:args.append((start,i));start=i+1
   i+=1
  if n>=len(args):raise Exception((name,n,s[m.start():i+1]))
  a,b=args[n]
  if n<len(args)-1:b+=1
  elif n>0:a-=1
  s=s[:a]+s[b:]
 return s
p=D/'hermit-cli/src/bin/hermit/verify.rs';s=p.read_text()
a=s.index('    /// How strictly the internal event stream');b=s.index('    pub compare_logs:',a);s=s[:a]+s[b:]
# Historical spec fields stay available to reject old report shapes; constructor has only canonical output.
a=s.index('        // Map the strictness label');b=s.index('        ComparisonSpec {',a)
s=s[:a]+'''        let strictness = LogCompareStrictness::Canonical;
        let strip_lines = false;
        let canonicalize_addresses = true;
        let full_trace = true;
        let exact_remainder = true;
        let log_scope = if diagnostic_full_trace { ComparedLogScope::FullTrace } else { ComparedLogScope::Info };
        let stripped_prefixes = PARITY_STRIPPED_PREFIXES;
        let canonicalizations = PARITY_CANONICALIZATIONS;
        let display_name = if diagnostic_full_trace { "BitwiseFullTraceV1" } else { "BitwiseInfoV1" };
'''+s[b:]
s=s.replace('        strictness: LogCompareStrictness,\n','') # constructor and test-helper parameters
s=s.replace('    strictness: LogCompareStrictness,\n','') # capture and setup parameters
s=s.replace('        requested.unwrap_or(match strictness {\n            LogCompareStrictness::Stripped => LevelFilter::DEBUG,\n            LogCompareStrictness::Canonical => LevelFilter::INFO,\n        })','        requested.unwrap_or(LevelFilter::INFO)')
s=s.replace('            ComparedLogScope::Deterministic => LogComparisonMode::Deterministic,','            ComparedLogScope::Deterministic => unreachable!("historical report scope is not an active comparator"),')
s=s.replace('                strip_lines: spec.strip_lines,\n','')
a=s.index('                // Thread the filter facts');b=s.index('                ..Default::default()',a);s=s[:a]+s[b:]
a=s.index('            // Bind the spec\'s recorded filter-absence');b=s.index('            let summary =',a);s=s[:a]+s[b:]
s=re.sub(r'^\s*strictness: LogCompareStrictness::(?:Canonical|Stripped),\n','',s,flags=re.M)
s=s.replace('                strictness,\n','')
# Keep synthetic old-report policy controls separate from active construction.
a=s.index('        let stripped = ComparisonSpec::new(',s.index('fn comparison_spec_maps'))
b=s.index('        let canonical = ComparisonSpec::new(',a);s=s[:a]+s[b:]
a=s.index('        let stripped = ComparisonSpec::new(',s.index('fn bitwise_parity_contract'))
b=s.index('        assert!(',a);s=s[:a]+'''        let stripped = ComparisonSpec {
            strictness: LogCompareStrictness::Stripped,
            strip_lines: true,
            exact_remainder: false,
            ..full
        };
'''+s[b:]
# Calls still carry historical strictness until this signature migration.
for name,n in [('ComparisonSpec::new',0),('verification_log_level',1),('setup_double_run',3),('compare_with_envelope',4),('compare_with',4)]:s=remove_arg(s,name,n)
s=s.replace('            LevelFilter::DEBUG,\n            "legacy stripped verification keeps its default"','            LevelFilter::INFO,\n            "ordinary verification captures its full INFO comparison scope"')
s=s.replace('// The default (stripped) comparison, matching what a bare `--verify` runs.','// The ordinary canonical comparison used by a bare `--verify`.')
s=s.replace('// The default `--verify` path is a stripped comparison; the verdict says so.','// Ordinary verification reports the canonical INFO comparison it performed.')
# The two ordinary success tests require the new default without changing guest status.
a=s.index('    fn identical_outputs_verify_successfully');b=s.index('    fn production_comparison_callers',a);chunk=s[a:b].replace('LogCompareStrictness::Stripped','LogCompareStrictness::Canonical').replace('assert!(outcome.comparison.strip_lines);','assert!(!outcome.comparison.strip_lines);').replace('assert!(!outcome.comparison.full_trace);','assert!(outcome.comparison.full_trace);');s=s[:a]+chunk+s[b:]
s=function(s,'stripped_matches_but_bitwise_diverges_on_numeric_only_log_difference','''    fn default_verification_diverges_on_numeric_only_log_difference() {
        let out = output(0, b"hello\\n", b"");
        let (log1, log2) = empty_logs();
        let path1 = log1.to_path_buf();
        let path2 = log2.to_path_buf();
        fs::write(&path1, detlog_with_value(100)).unwrap();
        fs::write(&path2, detlog_with_value(200)).unwrap();
        let outcome = compare(&out, log1, &out, log2).unwrap();
        assert_eq!(outcome.verdict, Verdict::Diverged);
        assert!(!outcome.verified());
        assert_eq!(outcome.guest_status, ExitStatus::Exited(0));
        assert_eq!(outcome.comparison.strictness, LogCompareStrictness::Canonical);
        assert_eq!(outcome.comparison.display_name, "BitwiseInfoV1");
        assert!(!outcome.comparison.strip_lines);
        assert!(outcome.comparison.full_trace);
        let report = verification_report(&outcome);
        assert!(!report.verified);
        assert_eq!(report.comparison.unwrap().strip_lines, Some(false));
        assert!(path1.exists(), "divergent run-1 log must be retained");
        assert!(path2.exists(), "divergent run-2 log must be retained");
        fs::remove_file(path1).unwrap();
        fs::remove_file(path2).unwrap();
    }''',indent='    ')
# No removed engine fields may be asserted as though still configurable.
s=function(s,'default_log_diff_opts_apply_no_line_filters','''    fn default_log_diff_opts_apply_no_line_filters() {
        use clap::Parser;
        let default = logdiff::LogDiffOpts::default();
        assert_eq!(default.comparison, LogComparisonMode::Info);
        assert!(default.canonicalize_addresses);
        for flag in ["--strip-lines", "--unsafe-strip-lines", "--ignore-lines=payload", "--skip-commit", "--skip-detlog", "--include-detlogs=syscall", "--git-diff"] {
            let error = logdiff::LogDiffOpts::try_parse_from(["log-diff", flag]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }''',indent='    ')
s=s.replace('Comparison strictness is carried\n    /// separately in [`Self::strictness`] so a quiet run can still be\n    /// bitwise-strict','The canonical comparison is always enabled so a quiet run is still\n    /// bitwise-strict')
s=s.replace('/// still selects INFO. Legacy stripped verification keeps its DEBUG default.','/// still selects INFO.')
p.write_text(s)
# Other source callsites: strictness remains a report fact, never an option.
for rel in ['hermit-cli/src/bin/hermit/run.rs','hermit-cli/src/bin/hermit/record_start.rs','hermit-cli/src/bin/hermit/backends.rs']:
 p=D/rel;s=p.read_text()
 s=s.replace('            strictness: self.verification_strictness(),\n','').replace('                strictness,\n','')
 if rel.endswith('run.rs'):
  s=function(s,'verification_strictness','''    fn verification_strictness(&self) -> LogCompareStrictness {
        LogCompareStrictness::Canonical
    }''',indent='    ')
  s=s.replace('            LogCompareStrictness::Stripped,','            LogCompareStrictness::Canonical,')
  s=s.replace('        let strictness = self.verification_strictness();\n','')
 if rel.endswith('record_start.rs'):
  a=s.index('        let strictness = if self.verify_strict');b=s.index('        let ((global1, log1)',a);s=s[:a]+s[b:]
  s=s.replace('use super::verify::LogCompareStrictness;\n','')
 for name,n in [('verification_log_level',1),('setup_double_run',3)]:s=remove_arg(s,name,n)
 p.write_text(s)
# Old wrapper use cases retain all INFO classes, including chaos diagnostics.
for rel in ['hermit-verify/src/trace_replay.rs','hermit-verify/src/chaos_replay.rs']:
 p=D/rel;s=p.read_text()
 s=re.sub(r'^\s*verify_(?:detlog_syscalls|detlog_syscall_results|detlog_others|commits): (?:true|false),\n','',s,flags=re.M)
 s=re.sub(r'^\s*ignore_lines: vec!\[.*?\],\n','',s,flags=re.M|re.S)
 if '            ignore_lines: if self.chaos' in s:
  a=s.index('            ignore_lines: if self.chaos');b=s.index('\n            },',a)+len('\n            },');s=s[:a]+s[b:]
 p.write_text(s)
