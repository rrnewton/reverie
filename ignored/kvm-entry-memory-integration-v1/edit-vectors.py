from pathlib import Path
p=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/reverie-kvm/src/memory.rs');s=p.read_text()
for name in ['write','zero']:
 i=s.index('    fn '+name+'_admitted(',s.index('impl UserMemory {'));a=s.index('{',i);sig=s[i:a].replace('&mut self','&self');s=s[:i]+sig+s[a:]
for owner,endmarker in [('GuestMemory','\nfn create_memory_backing'),('UserMemory','\n#[cfg(test)]\nmod tests')]:
 start=s.index('impl MemoryAccess for '+owner+' {');end=s.index(endmarker,start);body=s[start:end];memory='self' if owner=='GuestMemory' else 'self.memory'
 marker='            let requested = (read_from[source_index].len() - source_offset)';assert body.count(marker)==1
 body=body.replace(marker,'            let copy = '+memory+'.copy_access().map_err(|_| Errno::EIO)?;\n'+marker)
 marker='            let count = (read_from[source_index].len() - source_offset)';assert body.count(marker)==1
 body=body.replace(marker,'            let copy = '+memory+'.copy_access().map_err(|_| Errno::EIO)?;\n'+marker)
 prefix='            let count = self\n                .user_accessible_prefix(address, requested)';assert body.count(prefix)==2
 receiver='self.user()' if owner=='GuestMemory' else 'self'
 body=body.replace(prefix,'            let count = '+receiver+'\n                .user_accessible_prefix_admitted(address, requested, &copy)')
 body=body.replace('            if count == 0 {','            '+memory+'.check_copy_failure().map_err(|_| Errno::EIO)?;\n            if count == 0 {')
 for direction,arg in [('read','destination'),('write','source')]:
  raw=direction+('_raw' if owner=='GuestMemory' else '_translated_raw')
  old='            if self.'+raw+'(address, '+arg+').is_err() {'
  assert old in body
  body=body.replace(old,'            if self.'+raw+'_admitted(address, '+arg+', &copy).is_err() {\n                '+memory+'.check_copy_failure().map_err(|_| Errno::EIO)?;')
 marker='            total += count;';assert body.count(marker)==2
 body=body.replace(marker,marker+'\n            #[cfg(test)]\n            '+memory+'.after_vector_copy(total);\n            '+memory+'.check_copy_failure().map_err(|_| Errno::EIO)?;')
 s=s[:start]+body+s[end:]
p.write_text(s)
