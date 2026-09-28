// SOURCE 39ac9f8a55f1b4d17b8c4c0001ce6c4d8a763435:detcore/src/syscalls/files.rs lines 2688..2708; SHA256 b4f56303af3eab51be782582d95b8e368a3be7007157c1d6dcaca50ad6a4729a
    /// statx system call
    pub async fn handle_statx<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: syscalls::Statx,
    ) -> Result<i64, Error> {
        if guest.config().virtualize_metadata {
            // NB: let kernel handle error codes, it's not easy to do so without kernel
            // because there're many corner cases. i.e.: even access filepath from tracer
            // may cause tracer to hang under certain fuse filesystem (squashfs_ll).
            guest.inject(call).await?;
            let statptr = call.statx().ok_or(Errno::EFAULT)?;
            let mut memory = guest.memory();
            let stat = memory.read_value(statptr.0)?;
            let stat = self.determinize_stat(guest, stat, None).await?;
            memory.write_value(statptr.0, &stat.into())?;
            Ok(0)
        } else {
            Ok(self.record_or_replay(guest, call).await?)
        }
    }

