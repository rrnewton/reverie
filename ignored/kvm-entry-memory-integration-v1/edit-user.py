from pathlib import Path
p=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/reverie-kvm/src/memory.rs')
s=p.read_text();start=s.index('impl UserMemory {');end=s.index('\nimpl MemoryAccess for GuestMemory',start);u=s[start:end]
def span(text,name):
 i=text.index('    '+('pub(crate) ' if '    pub(crate) fn '+name+'(' in text else 'pub ' if '    pub fn '+name+'(' in text else '')+'fn '+name+'(')
 a=text.index('{',i);level=1;b=a+1
 while level:
  level+=(text[b]=='{')-(text[b]=='}');b+=1
 return i,a,b
def rewrite(name,new):
 global u
 i,a,b=span(u,name);u=u[:i]+new(u[i:a],u[a+1:b-1])+u[b:]
def admitted_sig(sig,name):
 sig=sig.replace('pub(crate) ','').replace('pub ','').replace('fn '+name+'(', 'fn '+name+'_admitted(')
 j=sig.index(') ->');sig=sig[:j].rstrip().rstrip(',')+', copy: &CopyAccess'+sig[j:]
 return sig
# Raw translated helpers never acquire admission a second time.
rewrite('translate',lambda sig,body:admitted_sig(sig,'translate').replace('copy: &CopyAccess','_copy: &CopyAccess')+'{'+body+'}')
for name in ['read_translated_raw','write_translated_raw']:
 def convert(sig,body,name=name):
  body=body.replace('self.translate(address, destination.len())?', 'self.translate_admitted(address, destination.len(), copy)?').replace('self.translate(address, source.len())?', 'self.translate_admitted(address, source.len(), copy)?')
  body=body.replace('self.memory.read_raw(physical, destination)', 'self.memory.read_raw_admitted(physical, destination, copy)').replace('self.memory.write_raw(physical, source)', 'self.memory.write_raw_admitted(physical, source, copy)')
  return admitted_sig(sig,name)+'{'+body+'}'
 rewrite(name,convert)
# Wrappers obtain one token; all nested address/permission/raw helpers use it.
wrappers={
 'write_injection':('self.write_translated_raw_admitted(address, source, copy)',None),
 'host_operand':('self.host_operand_admitted(address, length, copy)',None),
 'retain_translated_range':('self.retain_translated_range_admitted(address, length, copy)',None),
 'read':('self.read_admitted(guest_address, destination, copy)',None),
 'write':('self.write_admitted(guest_address, source, copy)',None),
 'write_user_prefix':('self.write_user_prefix_admitted(guest_address, source, partial, copy)',None),
 'zero':('self.zero_admitted(guest_address, length, copy)',None),
 'user_accessible_prefix':('self.user_accessible_prefix_admitted(guest_address, length, copy)',None),
 'user_writable_prefix':('self.user_writable_prefix_admitted(guest_address, length, copy)',None),
}
for name,(call,_) in wrappers.items():
 def convert(sig,body,name=name,call=call):
  wrapper=sig+'{\n        self.memory.with_copy(|copy| '+call+')\n    }'
  if name=='write_injection':return wrapper
  body=body.replace('self.read(address, &mut probe)?','self.read_admitted(address, &mut probe, copy)?').replace('self.retain_translated_range(address, length)','self.retain_translated_range_admitted(address, length, copy)')
  for old,new in [
   ('self.translate(address, length)?','self.translate_admitted(address, length, copy)?'),
   ('self.translate(guest_address, destination.len())?','self.translate_admitted(guest_address, destination.len(), copy)?'),
   ('self.translate(guest_address, source.len())?','self.translate_admitted(guest_address, source.len(), copy)?'),
   ('self.translate(guest_address, 1)?','self.translate_admitted(guest_address, 1, copy)?'),
   ('self.translate(guest_address, length)?','self.translate_admitted(guest_address, length, copy)?'),
   ('self.user_accessible_prefix(guest_address, destination.len())?','self.user_accessible_prefix_admitted(guest_address, destination.len(), copy)?'),
   ('self.user_accessible_prefix(guest_address, source.len())?','self.user_accessible_prefix_admitted(guest_address, source.len(), copy)?'),
   ('self.user_accessible_prefix(guest_address, length)?','self.user_accessible_prefix_admitted(guest_address, length, copy)?'),
   ('self.read_translated_raw(guest_address, destination)','self.read_translated_raw_admitted(guest_address, destination, copy)'),
   ('self.write_translated_raw(guest_address, source)','self.write_translated_raw_admitted(guest_address, source, copy)'),
   ('self.memory.write_raw(physical, &source[..length])?','self.memory.write_raw_admitted(physical, &source[..length], copy)?'),
   ('.zero_raw(self.translate_admitted(guest_address, length, copy)?, length)','.zero_raw_admitted(self.translate_admitted(guest_address, length, copy)?, length, copy)'),
  ]:body=body.replace(old,new)
  inner=admitted_sig(sig,name)
  if name in ['user_accessible_prefix','user_writable_prefix']:inner=inner.replace('copy: &CopyAccess','_copy: &CopyAccess')
  return wrapper+'\n\n'+inner+'{'+body+'}'
 rewrite(name,convert)
s=s[:start]+u+s[end:];p.write_text(s)
