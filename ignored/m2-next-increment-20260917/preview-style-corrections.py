from pathlib import Path
P=Path(__file__).resolve().parent/'preview'
p=P/'detcore/src/logdiff.rs';s=p.read_text()
s=s.replace('/// host addresses to first-appearance ordinals. Scheduler turns, virtual time, syscall\n/// values, counts, flags, and every other substantive byte are preserved.','/// host addresses to first-appearance ordinals. Scheduler turns, virtual time,\n/// syscall values, counts, flags, and every other substantive byte are preserved.')
s=s.replace('/// other numbers stay exact. Record position is not part of the message and remains a separate\n/// observation.','/// other numbers stay exact. Record position is not part of the message and\n/// remains a separate observation.')
for name in ['default_comparison_preserves_fields_after_tmp_paths','default_comparison_preserves_differing_tmp_paths']:
 start=s.index('    fn '+name+'(');end=s.index('\n    }',start)+6
 method=s[start:end];a=method.index('        for (left, right) in [(');b=method.index('        )] {',a)
 pairs=method[a:b].replace('        for (left, right) in [(', '        let (left, right) = (',1)+'        );'
 tail=method[b+len('        )] {\n'):]
 close=tail.index('        }\n        Ok(())');body=tail[:close]
 body='\n'.join(line[4:] if line.startswith('    ') else line for line in body.split('\n'))
 replacement=method[:a]+pairs+'\n'+body+tail[close+len('        }\n'):]
 s=s[:start]+replacement+s[end:]
p.write_text(s)
p=P/'hermit-cli/src/bin/hermit/verify.rs';s=p.read_text().replace('    /// enabled, so a quiet run is still bitwise-strict — the two knobs were historically conflated behind a single\n    /// `verbose` flag, which made the only bitwise comparison also the loudest.','    /// enabled, so a quiet run is still bitwise-strict. The two knobs were\n    /// historically conflated behind a single `verbose` flag, which made the\n    /// only bitwise comparison also the loudest.')
p.write_text(s)
print('Removed two single-element loops and wrapped three comments in owned previews.')
