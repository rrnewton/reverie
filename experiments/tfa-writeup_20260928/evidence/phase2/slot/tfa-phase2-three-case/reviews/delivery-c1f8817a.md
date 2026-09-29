# Review prompt delivery for tfa-rev-c1f8817a

`agentctl start ... --file prompt-c1f8817a.md` exited 76 (`possibly_submitted`,
message 01790336519576203338-812598, quarantined under the agent's
`queue/failed/`): the paste landed but Enter was not sent. The pane
(`delivery-c1f8817a-pre-enter.txt`) showed one `[Pasted Content 6806 chars]` in
the composer and no started turn, both at once and 20 s later. The prompt file is
6819 bytes, which is 6806 characters once its multibyte characters (· and —)
and trailing newline are accounted for. Automation was paused, a
single `enter` key was sent to pane wV:p19, and automation was resumed. The
reviewer then began its turn (`delivery-c1f8817a-post-enter.txt`). The prompt
was not resent and no second request was created.
