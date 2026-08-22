set confirm off
set pagination off
set sysroot /
set debuginfod enabled off
set debug remote 1
target remote :1303
break bkpt
continue
print global
continue
