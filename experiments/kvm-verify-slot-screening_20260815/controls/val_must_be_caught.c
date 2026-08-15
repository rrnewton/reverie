#include <stdio.h>
int main(void){
  long total = 0;
  for (int i = 1; i <= 8; ++i) total += i;
  if (total != 36) { fprintf(stderr,"bad total\n"); return 1; }
  printf("total=%ld\n", total);
  return 0;
}
