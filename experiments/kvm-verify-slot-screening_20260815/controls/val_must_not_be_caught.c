#include <stdio.h>
int main(void){
  long total = 0;
  for (int i = 1; i <= 8; ++i) total += i;
  printf("total=%ld\n", total);   /* expected 36 -- only in this comment */
  return 0;
}
