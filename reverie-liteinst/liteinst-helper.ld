SECTIONS
{
  .liteinst_helper ALIGN(4096) :
  {
    __reverie_liteinst_helper_page_start = .;
    KEEP(*(.liteinst_helper))
    __reverie_liteinst_helper_code_end = .;
    . = ALIGN(4096);
    __reverie_liteinst_helper_page_end = .;
  }
}
INSERT AFTER .text;

ASSERT(
  (!DEFINED(__reverie_liteinst_requires_runtime_helper) &&
    !DEFINED(__reverie_liteinst_downstream_requires_runtime_helper)) ||
    DEFINED(reverie_liteinst_host_install_helper),
  "required LiteInst runtime helper anchor is missing"
);
ASSERT(
  !DEFINED(reverie_liteinst_host_install_helper) ||
    (DEFINED(reverie_liteinst_install_site_for_ptrace) &&
      reverie_liteinst_host_install_helper ==
        reverie_liteinst_install_site_for_ptrace),
  "legacy LiteInst install symbol differs from authenticated helper entry"
);
ASSERT(
  !DEFINED(reverie_liteinst_host_install_helper) ||
    __reverie_liteinst_helper_code_end > __reverie_liteinst_helper_page_start,
  "LiteInst ptrace helper section is empty"
);
ASSERT(
  DEFINED(reverie_liteinst_host_install_helper) ||
    __reverie_liteinst_helper_code_end == __reverie_liteinst_helper_page_start,
  "LiteInst ptrace helper section exists without its runtime anchor"
);
ASSERT(
  !DEFINED(reverie_liteinst_host_install_helper) ||
    (DEFINED(reverie_liteinst_install_site_for_ptrace_body) &&
      reverie_liteinst_install_site_for_ptrace_body >= __reverie_liteinst_helper_page_start &&
      reverie_liteinst_install_site_for_ptrace_body < __reverie_liteinst_helper_code_end),
  "LiteInst ptrace helper body lies outside its isolated section"
);
ASSERT(
  !DEFINED(reverie_liteinst_host_install_helper) ||
    __reverie_liteinst_helper_code_end - __reverie_liteinst_helper_page_start <= 4096,
  "LiteInst ptrace helper body exceeds its isolated page"
);
ASSERT(
  !DEFINED(reverie_liteinst_host_install_helper) ||
    __reverie_liteinst_helper_page_end - __reverie_liteinst_helper_page_start == 4096,
  "LiteInst ptrace helper section does not occupy exactly one page"
);
