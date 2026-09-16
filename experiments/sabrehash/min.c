#include <stdio.h>
#include <stdlib.h>
int main(void){ char *p = malloc(4096); if(!p) return 1; p[0]=1; free(p); printf("min-ok\n"); return 0; }
