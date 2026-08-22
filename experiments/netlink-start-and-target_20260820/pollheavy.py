import os, select, socket
# A poll-heavy but realistic shape: a poller that mostly finds nothing ready.
r, w = os.pipe()
p = select.poll(); p.register(r, select.POLLIN)
ready = 0
for i in range(2000):
    if p.poll(0): ready += 1
    if i % 500 == 0:
        os.write(w, b"x"); os.read(r, 1)
print("polls=2000 ready=%d" % ready)
