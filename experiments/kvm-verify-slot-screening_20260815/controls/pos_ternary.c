#include <stdio.h>
int main(void){ int ok=0; for(int i=0;i<4;i++){ if(i==i) ok++; } puts("ternary-ok"); return ok == 4 ? 0 : 1; }
