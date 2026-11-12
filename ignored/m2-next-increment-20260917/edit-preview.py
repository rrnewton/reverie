from pathlib import Path
import re
D=Path(__file__).parent/'preview'
def read(p):return (D/p).read_text()
def write(p,s):(D/p).write_text(s)
def cut(s,a,b):
 i=s.index(a);j=s.index(b,i);return s[:i]+s[j:]
def function(s,name,replacement,indent=''):
 m=re.search(r'^'+re.escape(indent)+r'(?:pub(?:\([^)]*\))? )?fn '+re.escape(name)+r'\b',s,re.M)
 assert m,name
 e=s.index('\n'+indent+'}',m.start())+len(indent)+2
 return s[:m.start()]+replacement+s[e:]
p='detcore/src/logdiff.rs';s=read(p)
s=s.replace('use std::process::Command;\n','').replace('use std::str::FromStr;\n','').replace('use tempfile::NamedTempFile;\n','')
s=s.replace('    /// Compare deterministic Detcore and scheduler messages.\n    #[default]\n    Deterministic,\n','')
s=s.replace('    Info,','    #[default]\n    Info,',1)
s=cut(s,'    /// UNSAFE: strips numbers','    /// Canonicalize host memory')
s=s.replace('    /// (see `canonicalize_addresses_in_line`). Unlike [`Self::strip_lines`],','    /// (see `canonicalize_addresses_in_line`). This')
s=s.replace('    #[clap(skip)]\n    pub canonicalize_addresses: bool,','    #[clap(skip = true)]\n    pub canonicalize_addresses: bool,')
s=cut(s,'    /// Before comparison, filter out lines','    /// Show this many completed syscalls')
s=cut(s,'    /// Do not consider "COMMIT"','}\n\nimpl LogDiffOpts')
s=cut(s,'impl LogDiffOpts {','#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum LogNormalization')
s=s.replace('    Stripped,\n','',1)
s=s.replace('        let normalization = if options.strip_lines {\n            LogNormalization::Stripped\n        } else if options.canonicalize_addresses {','        let normalization = if options.canonicalize_addresses {')
s=cut(s,'            (LogComparisonMode::Deterministic,','            (LogComparisonMode::Info, LogNormalization::Exact)')
s=cut(s,'            (LogComparisonMode::Info, LogNormalization::Stripped)','            (LogComparisonMode::Info, LogNormalization::Canonical)')
s=cut(s,'            (LogComparisonMode::FullTrace, LogNormalization::Stripped)','            (LogComparisonMode::FullTrace, LogNormalization::Canonical)')
s=cut(s,'/// Indicates which DETLOG entries','/// N.B. we don\'t want to specify')
s=cut(s,'/// In fully-deterministic modes,','/// Wrap a host memory address')
s=s.replace('/// from [`strip_log_entry`]\'s `<ADDR>` erasure in one decisive way: erasure maps','/// from wholesale `<ADDR>` erasure in one decisive way: erasure maps')
s=s.replace('/// This is deliberately narrower than [`strip_log_entry`]. Syscall arguments,','/// This preserves syscall arguments,')
s=cut(s,'        LogNormalization::Stripped =>','        LogNormalization::Canonical =>')
s=function(s,'git_diff','')
s=function(s,'filter_ignored','')
# Remove historical deterministic selector helpers, which have no remaining production caller.
s=function(s,'is_internal_io_poll_commit','');s=function(s,'is_scheduler_committed_time','')
s=s.replace('opts.strip_lines || opts.canonicalize_addresses','opts.canonicalize_addresses')
s=re.sub(r'^\s*(?:strip_lines|ignore_lines|skip_commit|skip_detlog|git_diff): (?:false|true|Vec::new\(\)),\n','',s,flags=re.M)
s=re.sub(r'^\s*include_detlogs: vec!\[.*?\],\n','',s,flags=re.M|re.S)
s=re.sub(r'^\s*use crate::logdiff::DetLogFilter;\n','',s,flags=re.M)
s=s.replace('super::LogComparisonMode::Deterministic','super::LogComparisonMode::Info')
s=s.replace('    let all_a = filter_ignored(\n        extracted_a','    let all_a: Vec<_> = extracted_a').replace('    let all_b = filter_ignored(\n        extracted_b','    let all_b: Vec<_> = extracted_b')
s=s.replace('            .collect(),\n        &opts.ignore_lines,\n    );','            .collect();')
s=s.replace('    let detlogs_a = opts.filter_deterministic(&detcore_a);\n    let detlogs_b = opts.filter_deterministic(&detcore_b);\n','')
s=cut(s,'    writeln!(\n        w,\n        "Logs contain {} | {} DETLOG & scheduler COMMIT messages",','    let policy = LogComparisonPolicy::from_options(opts);')
s=cut(s,'    if policy.normalization == LogNormalization::Stripped {','    } else if policy.normalization == LogNormalization::Canonical {')
s=s.replace('    } else if policy.normalization == LogNormalization::Canonical {','    if policy.normalization == LogNormalization::Canonical {',1)
s=s.replace('        LogComparisonMode::Deterministic => ("DETLOG", &detlogs_a, &detlogs_b),\n','')
s=cut(s,'    let diff_found = if opts.git_diff {','    } else {\n        diff_vecs(').replace('    } else {\n        diff_vecs(','    let diff_found = diff_vecs(',1)
s=s.replace('            &right_syscalls,\n        )?\n    };','            &right_syscalls,\n        )?;',1)
s=s.replace('    let str_a = String::from_utf8_lossy(&vec_a);\n    let str_b = String::from_utf8_lossy(&vec_b);','    let str_a = std::str::from_utf8(&vec_a)\n        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;\n    let str_b = std::str::from_utf8(&vec_b)\n        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;')
# Fixed policy absence is enforced by the absence of active options, tested by Clap rejection.
s=re.sub(r'^\s*assert!\(!options\.(?:strip_lines|skip_commit|skip_detlog|git_diff)\);\n','',s,flags=re.M)
s=s.replace('        assert!(options.ignore_lines.is_empty());\n','')
s=re.sub(r'        assert_eq!\(\n            options.include_detlogs,.*?\n        \);\n','',s,flags=re.S)
s=s.replace('Comparing DETLOG messages','Comparing INFO messages')
write(p,s)
# Eliminate COMMIT erasure at the actual integration helper.
p='detcore/tests/testutils/src/lib.rs';s=read(p);s=s.replace('let str_a = detcore::logdiff::strip_log_entry(&x[ix]);','let str_a = &x[ix];').replace('let str_b = detcore::logdiff::strip_log_entry(&filtered[ix]);','let str_b = &filtered[ix];');write(p,s)
# CLI compatibility flag stays; defaults take the already-fixed all-records comparator.
p='hermit-cli/src/bin/hermit/logdiff.rs';s=read(p)
s=s.replace('        !self.more.strip_lines\n            && self.more.limit == defaults.limit','        self.more.limit == defaults.limit')
for line in ['            && self.more.ignore_lines.is_empty()\n','            && !self.more.skip_commit\n','            && !self.more.skip_detlog\n','            && !self.more.git_diff\n','            && self.more.include_detlogs == defaults.include_detlogs\n']:s=s.replace(line,'')
s=cut(s,'        if self.canonical_info || self.json.is_some() {','        if record_envelope.policy() == RecordEnvelopePolicy::CrossBackendDetcoreV1 {')
s=s.replace('if (self.canonical_info || self.json.is_some())\n            && record_envelope.policy()','if record_envelope.policy()')
s=s.replace('let fixed_bitwise_info_v1 = (self.canonical_info || self.json.is_some())\n            && record_envelope.policy()','let fixed_bitwise_info_v1 = record_envelope.policy()')
s=function(s,'canonical_comparison_is_unrelaxed','')
s=s.replace('        logdiff::LogComparisonMode::Deterministic => "deterministic",\n','')
s=cut(s,'    let included_detlog_kinds = options','    LogDiffReport {')
s=s.replace('unsafe_strip_lines: options.strip_lines','unsafe_strip_lines: false').replace('ignored_line_substrings: options.ignore_lines.clone()','ignored_line_substrings: Vec::new()').replace('skip_commit: options.skip_commit','skip_commit: false').replace('skip_detlog: options.skip_detlog','skip_detlog: false').replace('git_diff: options.git_diff','git_diff: false')
s=s.replace('            included_detlog_kinds,','            included_detlog_kinds: vec!["syscall".into(), "syscall_result".into(), "other".into()],')
write(p,s)
p='hermit-cli/src/bin/hermit/analyze/phases.rs';s=read(p);s=s.replace('        ldopts.more.ignore_lines = vec!["CHAOSRAND".to_string(), "SCHEDRAND".to_string()];\n','');write(p,s)
# Wrapper options cannot opt out of log classes or of the entire comparison.
p='hermit-verify/src/common/verify.rs';s=read(p);s=cut(s,'    /// Whether to skip commits','}\n\nimpl LogDiffOptions')
a=s.index('        let mut result: Vec<String>');b=s.index('\n    }\n}',a);s=s[:a]+'        vec!["--canonical-info".to_owned(), format!("--syscall-history={}", self.syscall_history)]'+s[b:]
# Use the actual Command constructor in both production and the argv control.
a=s.index('        let mut command = std::process::Command::new(&self.hermit_bin);',s.index('    pub fn verify_logs'))
b=s.index('\n        println!',a);s=s[:a]+'        let mut command = self.log_diff_command(left, right, options);\n'+s[b:]
idx=s.index('    pub fn verify_logs(');s=s[:idx]+'''    fn log_diff_command(
        &self,
        left: &RunEnvironment,
        right: &RunEnvironment,
        options: LogDiffOptions,
    ) -> std::process::Command {
        let mut command = std::process::Command::new(&self.hermit_bin);
        command.args(self.build_command_args(left, right, options));
        command
    }

'''+s[idx:]
for name in ['test_build_command_args_ignore_lines_provided','test_build_command_args_ignore_lines_not_provided']:
 s=function(s,name,'''    fn '''+name+'''() -> anyhow::Result<()> {
        let env = TemporaryEnvironmentBuilder::new().run_count(2).build()?;
        let verify = Verify::new(PathBuf::from("hermit"));
        let command = verify.log_diff_command(&env.runs()[0], &env.runs()[1], LogDiffOptions { syscall_history: 5 });
        assert_eq!(command.get_program(), OsStr::new("hermit"));
        assert_eq!(command.get_args().collect::<Vec<_>>(), vec![
            OsStr::new("log-diff"), OsStr::new("--canonical-info"), OsStr::new("--syscall-history=5"),
            env.runs()[0].log_file_path.as_os_str(), env.runs()[1].log_file_path.as_os_str(),
        ]);
        Ok(())
    }''',indent='    ')
write(p,s)
p='hermit-verify/src/use_case.rs';s=read(p)
for f in ['verify_detlog_syscalls','verify_detlog_syscall_results','verify_detlog_others','verify_commits']:
 s=re.sub(r'^    pub '+f+r': bool,\n','',s,flags=re.M);s=re.sub(r'^            '+f+r': true,\n','',s,flags=re.M)
s=s.replace('    pub ignore_lines: Vec<String>,\n','').replace('            ignore_lines: Vec::new(),\n','');s=cut(s,'impl UseCaseOptions {','pub trait UseCase');write(p,s)
p='hermit-verify/src/use_case/run_usecase.rs';s=read(p);a=s.index('    if options.should_log_diff()');b=s.index('    if options.verify_exit_statuses',a);s=s[:a]+'''    result &= verify.verify_logs(left, right, LogDiffOptions { syscall_history: 5 })?;

'''+s[b:];write(p,s)
