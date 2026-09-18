/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Query-preserving x86-64 `__vdso_getrandom` routing for LiteInst.
//!
//! The Linux vDSO entry has a five-argument query ABI in addition to its
//! ordinary entropy operation. Replacing the entry with a three-argument
//! syscall would silently break the loader query. This module accepts one
//! complete, byte-for-byte reviewed 8,192-byte vDSO image. Its query gate
//! and all argument,
//! page-boundary, readiness, zero-length, and in-use checks remain intact.
//! The first state write becomes a direct jump to the function's existing
//! general fallback. Its syscall-number load, syscall, following tail jump,
//! query path, and sole SGX indirect call remain byte-identical. With the
//! after-loader all-Trace seccomp policy, that existing raw syscall reaches the
//! ordinary ptrace syscall path without an in-process callback frame.

use std::io;

const NORMAL_BRANCH_OFFSETS: [usize; 3] = [0x30, 0x34, 0x39];
const QUERY_BRANCH_OFFSET: usize = 0x3f;
const QUERY_BODY_OFFSET: usize = 0x3db;
const QUERY_RETURN_JUMP_OFFSET: usize = 0x42a;
const NORMAL_PATH_OFFSET: usize = 0x45;
pub(super) const REDIRECT_OFFSET: usize = 0x99;
pub(super) const GENERAL_FALLBACK_OFFSET: usize = 0x456;
pub(super) const GENERAL_SYSCALL_OFFSET: usize = 0x45b;
pub(super) const GENERAL_RETURN_JUMP_OFFSET: usize = 0x45d;
pub(super) const COMMON_EPILOGUE_OFFSET: usize = 0x3cc;

pub(super) const KNOWN_VDSO_IMAGE_BYTES: usize = 0x2000;
#[cfg(test)]
pub(super) const FUNCTION_OFFSET: usize = 0x1050;
pub(super) const SGX_TARGET_LOAD_OFFSET: usize = 0x16c4;
pub(super) const SGX_INDIRECT_CALL_OFFSET: usize = 0x16cb;
pub(super) const SGX_GUARD_DISPLACED_LEN: usize = SGX_INDIRECT_CALL_OFFSET - SGX_TARGET_LOAD_OFFSET;
pub(super) const SGX_GUARD_SOURCE: [u8; 9] = [0x48, 0x8b, 0x40, 0x18, 0x0f, 0xae, 0xe8, 0xff, 0xd0];

pub(super) const NORMAL_BRANCH_INSTRUCTION: [u8; 5] = [0x41, 0xc6, 0x86, 0x89, 0x00];
pub(super) const FORCE_GENERAL_FALLBACK: [u8; 5] = [0xe9, 0xb8, 0x03, 0x00, 0x00];
pub(super) const GENERAL_FALLBACK_INSTRUCTION: [u8; 5] = [0xb8, 0x3e, 0x01, 0x00, 0x00];
pub(super) const INTERNAL_SYSCALL_WORD: [u8; 8] = [0x0f, 0x05, 0xe9, 0x6a, 0xff, 0xff, 0xff, 0x90];

// Exact bytes of the sole complete vDSO image admitted for the getrandom
// stopped redirect. Hex is only a compact source representation: admission compares
// every decoded byte, so the GNU build ID is never used as an integrity
// shortcut.
const KNOWN_VDSO_IMAGE_HEX: &str = concat!(
    "7f454c4602010100000000000000000003003e000100000000000000000000004000000000000000481800000000000000000000400038000600400010000f00",
    "0100000005000000000000000000000000000000000000000000000000000000aa17000000000000aa1700000000000000100000000000000200000004000000",
    "f804000000000000f804000000000000f804000000000000c000000000000000c00000000000000008000000000000000400000004000000d005000000000000",
    "d005000000000000d00500000000000054000000000000005400000000000000040000000000000050e574640400000024060000000000002406000000000000",
    "24060000000000004c000000000000004c00000000000000040000000000000051e5746406000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000053e57464040000000000000000000000000000000000000000000000000000000000000000000000",
    "000000000000000000000000000000000e0000000e00000000000000080000000000000004000000000000000d0000000000000006000000000000000b000000",
    "000000000c0000000000000005000000000000000000000000000000000000000000000000000000020000000100000003000000070000000900000000000000",
    "0a000000000000000300000001000000040000001a0000000120000b411000000004081c0020008080001080420000c200000004800000000100000007000000",
    "0b00000000ca1bb00c8e1e82528f3068864b85e67e55dd71db109a9e18a3436e94789e7cbe59a5844bcc086c26b062656c5887ffa7f08ff80000000000000000",
    "000000000000000000000000000000000100000012000c00e007000000000000bd020000000000001500000012000c00a00a0000000000002c00000000000000",
    "3600000012000c00200f00000000000070000000000000004a00000022000c00e007000000000000bd020000000000005c00000022000c00d00a000000000000",
    "4c040000000000006a00000022000c00200f00000000000070000000000000002100000012000c00d00a0000000000004c040000000000005700000022000c00",
    "a00a0000000000002c000000000000008c00000012000c0050100000000000006204000000000000a700000012000c0040160000000000009c00000000000000",
    "7700000012000c0020100000000000002b000000000000008500000022000c0020100000000000002b000000000000009d00000022000c005010000000000000",
    "6204000000000000005f5f7664736f5f67657474696d656f66646179005f5f7664736f5f74696d65005f5f7664736f5f636c6f636b5f67657474696d65005f5f",
    "7664736f5f636c6f636b5f6765747265730067657474696d656f666461790074696d6500636c6f636b5f67657474696d6500636c6f636b5f676574726573005f",
    "5f7664736f5f67657463707500676574637075005f5f7664736f5f67657472616e646f6d0067657472616e646f6d005f5f7664736f5f7367785f656e7465725f",
    "656e636c617665006c696e75782d7664736f2e736f2e31004c494e55585f322e3600000002000200020002000200020002000200020002000200020002000000",
    "0100010001000100a1bfee0d140000001c000000c0000000000000000100000002000100f675ae031400000000000000d0000000000000000e00000000000000",
    "c0000000000000001e000000000000000200000000000000060000000000000078020000000000000b0000000000000018000000000000000500000000000000",
    "c8030000000000000a00000000000000da00000000000000f5feff6f00000000080200000000000004000000000000009001000000000000f0ffff6f00000000",
    "a204000000000000fcffff6f00000000c004000000000000fdffff6f000000000200000000000000000000000000000000000000000000000000000000000000",
    "657870616e642033322d62797465206b0600000004000000000000004c696e7578000000030107000600000001000000000100004c696e757800000000000000",
    "040000001400000003000000474e55006992fb1026cf3eee40bee09fe724ba1f3bbf7f6a011b033b4800000008000000bc010000640000007c04000094000000",
    "ac040000b4000000fc080000e40000006c0900000c010000fc090000340100002c0a0000540100001c100000840100001400000000000000017a520001781001",
    "1b0c0708900100002c0000001c00000050010000bd02000000410e108602430d064d83078c068d058e048f03035d010c0708410c061000001c0000004c000000",
    "e00300002c00000000410e108602430d06670c07080000002c0000006c000000f00300004c04000000410e108602430d064d83078c068d058e048f03036e010c",
    "0708410c06100000240000009c000000100800007000000000410e108602430d06025f0c0708410c06104b0c0708000024000000c4000000580800008b000000",
    "00410e108602430d06027d0c0708410c06100000000000001c000000ec000000c00800002b00000000410e108602430d06660c07080000002c0000000c010000",
    "d00800006204000000410e108602430d064d83078c068d058e048f0303c9030c0708410c06100000240000003c010000900e00009c00000000410e108602430d",
    "06418303024e0c0708410c061000000000000000000000000000000000000000554889e54157415641554154534883ec104885ff0f843f01000048bbffffffff",
    "ffffff7f49beffffffffffffff3f4d8d7e01448b25e797ffff41f6c401755f8b05df97ffff83f80175680f01f9669048c1e2204809c24821da4c8b05f097ffff",
    "482b15c197ffff483b15c297ffff776f8b05ca97ffff480fafd04c01c20fb60dc097ffff48d3ea4989d0488b0db797ffff8b058997ffff4439e07596eb7f813d",
    "7c97ffffffffff7f0f84fc000000f390eb8083f8020f85b8010000488975d04989fde8e90600004c89ef488b75d04885c00f889c0100004889c2e97affffff4c",
    "85fa740c0fb60d5997ffff49d3e8eb9a4c21f28b054797ffff0fb60d4497ffff48f7e24c01c04883d200480fadd048d3eaf6c140480f44d0e96affffff4c8945",
    "c831c04981f800ca9a3b72184981c0003665c44c8945c8ffc04981f8ffc99a3b77ea89c04801c84889074969c0d34d621048c1e8264889470831c04885f6750f",
    "4883c4105b415c415d415e415f5dc3813dab96ffffffffff7f488d0db0afffff488d15a99fffff480f44d18b0a890e488d0d9eafffff488d15979fffff480f44",
    "d18b0a894e04ebb8f390448b256fa6ffff41f6c40175f18b0567a6ffff83f80175540f01f9669048c1e2204809c24821da4c8b0578a6ffff482b1549a6ffff48",
    "3b154aa6ffff77508b0552a6ffff480fafd04c01c20fb60d48a6ffff48d3ea4989d0488b053fa6ffff8b0d11a6ffff4439e17596eb6983f8027558488975d049",
    "89fde8890500004c89ef488b75d04885c078404889c2eb994c85fa740c0fb60d00a6ffff49d3e8ebb94c21f28b05eea5ffff0fb60deba5ffff48f7e24c01c048",
    "83d200480fadd048d3eaf6c140480f44d0eb8cb8600000000f05e9e1feffff480305c295ffff4c0305c395ffff4c8945c831c94981f800ca9a3b0f82a4feffff",
    "4981c0003665c44c8945c8ffc14981f8ffc99a3b77ea89c9e987feffff0f1f00554889e5488d055595ffff31c9813d4d95ffffffffff7f0f94c1c1e10c488b44",
    "01284885ff74034889075dc30f1f4000554889e54157415641554154534883ec5883ff170f87a4030000b80100000089f9d3e0a9830800000f84520100004c8d",
    "3dfb94ffff48bbffffffffffffff7f4189fd49c1e5044f8d342f4983c62849b9ffffffffffffff3f4d8d5101458b2741f6c401754c418b470483f80175550f01",
    "f9669048c1e2204809c24821da4d8b4608492b5708493b57107777418b4720480fafd04c01c2410fb64f2448d3ea4989d0498b0e418b074439e075b0e9890000",
    "0041817f04ffffff7f0f841d010000f390eb994c8955b883f8020f85ee020000488975b0897dd4e8e40300008b7dd4488b75b04885c00f88d20200004889c249",
    "b9ffffffffffffff3f4c8b55b8e97bffffff4c85d2740a410fb64f2449d3e8eb904c21ca418b4720410fb64f2448f7e24c01c04883d200480fadd048d3eaf6c1",
    "40480f44d0e964ffffff4c8945c831c04981f800ca9a3b721c31d24981c0003665c44c8945c8ffc24981f8ffc99a3b77ea89d2eb0231d24801ca4889164c8946",
    "084883c4585b415c415d415e415f5dc3a8600f843202000089fa48c1e204488d3d9b93ffff488d0c3a4883c128448b058c93ffff41f6c001751e488b01488906",
    "488b410848894608448b0d7193ffff31c04539c175d7eba9813d6293ffffffffff7f0f8401020000f390ebc183ff04b8e810000041bb001000004c0f44d8488d",
    "054ba4ffff4c8d255ca3ffff4c0f44e0b8ec100000b904100000480f44c848894d80b8f0100000b908100000480f44c848894d88b8f8100000b910100000480f",
    "44c848894d90b808110000b920100000480f44c848894da0b80c110000b924100000480f44c848894da84d01ec4c8d2dcc92ffff478b3c2b41f6c7017563488b",
    "4580428b042883f801755a0f01f9669048c1e2204809c24821da4d8b442408488b45884a2b1428488b45904a3b1428777f488b45a0428b0428480fafd04c01c2",
    "488b45a8420fb60c2848d3ea4989d0498b0c24438b042b4439f87598e9aa000000f390eb8f4c8965c04c895d984c8955b883f8020f85d40000004d89cc488975",
    "b0897dd4e8c70100008b7dd4488b75b04885c00f88b50000004889c24d89e14c8b55b84c8b5d984c8b65c0e96affffff4c8965c04d89dc4d89d34c85d2740e48",
    "8b45a8420fb60c2849d3e8eb2f4c21ca488b45a0428b0428488b4da8420fb60c2948f7e24c01c04883d200480fadd048d3eaf6c140480f44d04989d04d89da4d",
    "89e34c8b65c0e944ffffff49030e4d0346084c8945c831c04981f800ca9a3b721c31d24981c0003665c44c8945c8ffc24981f8ffc99a3b77ea89d2eb0231d248",
    "01d148890ee9b3fdffffa810750f4863ffb8e40000000f05e9a4fdffff4c8d3d4492ffffe95cfcffff488d043a480528100000448b0546a1ffff41f6c0017515",
    "488b10488b7808448b0d32a1ffff4539c175e0eb04f390ebda4803114803790848897dc831c04881ff00ca9a3b721c31c94881c7003665c448897dc8ffc14881",
    "ffffc99a3b77ea89c9eb0231c94801ca48891648897e08e925fdffff0f1f4000554889e583ff17775bb80100000089f9d3e0a993080000742231c0813dbf90ff",
    "ffffffff7f488d0db490ffff0f94c0c1e00c8b8c0818090000eb15b940420f00a860750cb901000000a90000ff00741431c04885f6740b48c706000000004889",
    "4e085dc34863ffb8e50000000f055dc3554889e5508b3565d0fffff6057bd0ffff01746e83e6fe0f01f9669048c1e2204809d0482b054ed0ffff8b3d58d0ffff",
    "0fb61555d0ffff89d1f6d94989c049d3e889d148d3e084d2490f48c048897df848f765f8480facd020488b0d20d0ffff8b150ad0ffff39d689d6759f4801c148",
    "b8ffffffffffffff7f4821c84883c4085dc348c7c0ffffffffebf19090909090554889e5b87b000000f30fc7f84885ff740a89c181e1ff0f0000890f4885f674",
    "0648c1e80c890631c05dc39090909090554889e54157415641554154534883ec304989ce4881fe00f0ff7f41bc00f0ff7f4c0f42e648c745a8000000004885f6",
    "751385d2750f4885ff750a4983f8ff0f84960300004489f025ff0f00003d700f00000f87d403000083fa070f87f50300004981f8900000000f85e8030000803d",
    "43afffff000f84db0300004885f60f84b10300004180be89000000000f85c4030000488975c08955d441c68689000000014d8d6e60498b86800000004531c94c",
    "8d7da84c8965c848897db8488b0deeaeffff4839c84c894db00f85b7020000410fb686880000004889fbeb1db9020000004c89f74c89ee4c89fae88103000041",
    "c686880000000031c0b9600000004829c14c39e1490f43cc4885c90f84140200004c01f04883f908721e488d79f889faf7d2f6c218751c4889de4889ca4883ff",
    "18734de9970000004889ca4889dee98c00000089fec1ee03ffc683e60331d24531c04e8b0cc04e890cc34ac704c00000000049ffc04883c2f84c39c675e44889",
    "de4829d64829d04801ca4883ff18724f488b3848893e48c70000000000488b780848897e0848c7400800000000488b781048897e1048c7401000000000488b78",
    "1848897e1848c74018000000004883c6204883c0204883c2e04883fa0777b14883fa040f8292000000488d7afc4189f841f7d041f6c00c74394189f841c1e802",
    "41ffc04183e0034531c94531d2468b1c9046891c9642c704900000000049ffc24983c1fc4d39d075e44c29ce4c29c84c01ca4883ff0c72438b38893ec7000000",
    "00008b7804897e04c74004000000008b7808897e08c74008000000008b780c897e0cc7400c000000004883c6104883c0104883c2f04883fa0377bd4883fa020f",
    "8296000000488d7afe4189f841f7d041f6c00674394189f841d1e841ffc04183e0034531c94531d2460fb71c506646891c566642c70450000049ffc24983c1fe",
    "4d39d075e34c29ce4c29c84c01ca4883ff0672470fb73866893e66c70000000fb7780266897e0266c7400200000fb7780466897e0466c7400400000fb7780666",
    "897e0666c7400600004883c6084883c0084883c2f84883fa0177b94885d274080fb6108816c6000041008e880000004801cb4929cc4d85e474324983fc400f82",
    "a8fdffff4c89e148c1e9064889df4c89ee4c89fae8270100004c89e04883e0c04801c34183e43fe980fdffff498b8680000000483b0546acffff7454f645b001",
    "41b1014c8b65c8488b7db80f843afdffffe9c10000004989f849898e80000000be20000000b83e0100004c89ef31d20f054883f8200f858e00000041c6868800",
    "000060b8600000004c89c7e917fdffff41c6868900000000488b45c84883c4305b415c415d415e415f5dc348b8900000000300000049890641c7460828000000",
    "49c7460c0000000049c746140000000049c7461c0000000049c746240000000049c7462c0000000049c746340000000041c7463c0000000031c0eba048c7c0f2",
    "ffffffeb9731c0eb9349c78680000000000000004c89c741c68689000000008b55d4488b75c0b83e0100000f05e96affffff9090909090909090909090909090",
    "0f282df9f0ffff0f10360f107e10f3440f7e0248c7c001000000664c0f6ec8660f6fcd660f6fd6660f6fdf66410f6fe0b00a660ffeca660fefe1660f6fc4660f",
    "72f010660f72d410660febe0660ffedc660fefd3660f6fc2660f72f00c660f72d214660febd0660ffeca660fefe1660f6fc4660f72f008660f72d418660febe0",
    "660ffedc660fefd3660f6fc2660f72f007660f72d219660febd0660f70d239660f70db4e660f70e493660ffeca660fefe1660f6fc4660f72f010660f72d41066",
    "0febe0660ffedc660fefd3660f6fc2660f72f00c660f72d214660febd0660ffeca660fefe1660f6fc4660f72f008660f72d418660febe0660ffedc660fefd366",
    "0f6fc2660f72f007660f72d219660febd0660f70d293660f70db4e660f70e439fec80f850affffff660ffecd0f110f660ffed60f115710660ffedf0f115f2066",
    "410ffee00f11673066450fd4c14883c74048ffc90f85c5feffff66440fd602660fefc9660fefd2660fefdb660fefe4660feff6660fefff660fefc0c390909090",
    "554889e55389c883f802724883f8037743488b4d1048c7c32800000048833c190075314883c3084881fb0001000075ec488b19488d0d000000000f01d7488b5d",
    "10c743080400000048837b1800752131c05bc9c3b8eaffffffebf6488b5d1089430866897b0c6689730e48895310ebd84889e14889d84889e34883e30f4883e4",
    "f05050fc488b40180faee8ffd0488d641c1083f8007ebae96bffffff4ef1ffff9600000082020000050540f1ffff8d0000003b0000000503aaf2ffff82000000",
    "8202000005059cf2ffff790000003b00000005032af4ffff6e0000008202000005051cf4ffff650000003b00000005031bf6ffff5a0000008202000005050df6",
    "ffff510000003b00000005035bf8ffff460000008202000005054df8ffff3d0000003b0000000503c1f8ffff320000001602000004040faee80f310f01f90fae",
    "e80f310f01f90faee80f310f01f90faee80f310f01f90faee80f310f01f9f30fc7f80000000000000000002e74657874002e616c74696e7374725f7265706c61",
    "63656d656e74002e616c74696e737472756374696f6e73002e64796e737472002e65685f6672616d655f686472002e676e752e76657273696f6e002e64796e73",
    "796d002e676e752e68617368002e6e6f7465002e65685f6672616d65002e676e752e76657273696f6e5f64002e64796e616d6963002e7368737472746162002e",
    "726f6461746100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000005d00000005000000020000000000000090010000000000009001000000000000780000000000000003000000000000000400000000000000",
    "040000000000000059000000f6ffff6f020000000000000008020000000000000802000000000000700000000000000003000000000000000800000000000000",
    "0000000000000000510000000b000000020000000000000078020000000000007802000000000000500100000000000004000000010000000800000000000000",
    "18000000000000002e000000030000000200000000000000c803000000000000c803000000000000da0000000000000000000000000000000100000000000000",
    "000000000000000044000000ffffff6f0200000000000000a204000000000000a2040000000000001c0000000000000003000000000000000200000000000000",
    "020000000000000073000000fdffff6f0200000000000000c004000000000000c004000000000000380000000000000004000000020000000400000000000000",
    "000000000000000082000000060000000300000000000000f804000000000000f804000000000000c00000000000000004000000000000000800000000000000",
    "100000000000000095000000010000000300000000000000c005000000000000c005000000000000100000000000000000000000000000001000000000000000",
    "000000000000000063000000070000000200000000000000d005000000000000d005000000000000540000000000000000000000000000000400000000000000",
    "000000000000000036000000010000000200000000000000240600000000000024060000000000004c0000000000000000000000000000000400000000000000",
    "00000000000000006900000001000000020000000000000070060000000000007006000000000000640100000000000000000000000000000800000000000000",
    "000000000000000001000000010000000600000000000000e007000000000000e007000000000000fc0e00000000000000000000000000001000000000000000",
    "00000000000000001d000000010000001200000000000000dc16000000000000dc160000000000009a0000000000000000000000000000000100000000000000",
    "0e0000000000000007000000010000000600000000000000761700000000000076170000000000002c0000000000000000000000000000000100000000000000",
    "00000000000000008b0000000300000000000000000000000000000000000000aa170000000000009d0000000000000000000000000000000100000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);

// Extracted from the function at offset 0x1050 in the retained Linux vDSO
// whose complete image has GNU build ID 6992fb1026cf3eee40bee09fe724ba1f3bbf7f6a.
// Runtime acceptance binds both these 1,122 function bytes and every byte
// of the complete 8,192-byte image above, never the note alone.
pub(super) const KNOWN_GETRANDOM: &[u8; 1122] = &[
    0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x53, 0x48, 0x83, 0xec,
    0x30, 0x49, 0x89, 0xce, 0x48, 0x81, 0xfe, 0x00, 0xf0, 0xff, 0x7f, 0x41, 0xbc, 0x00, 0xf0, 0xff,
    0x7f, 0x4c, 0x0f, 0x42, 0xe6, 0x48, 0xc7, 0x45, 0xa8, 0x00, 0x00, 0x00, 0x00, 0x48, 0x85, 0xf6,
    0x75, 0x13, 0x85, 0xd2, 0x75, 0x0f, 0x48, 0x85, 0xff, 0x75, 0x0a, 0x49, 0x83, 0xf8, 0xff, 0x0f,
    0x84, 0x96, 0x03, 0x00, 0x00, 0x44, 0x89, 0xf0, 0x25, 0xff, 0x0f, 0x00, 0x00, 0x3d, 0x70, 0x0f,
    0x00, 0x00, 0x0f, 0x87, 0xd4, 0x03, 0x00, 0x00, 0x83, 0xfa, 0x07, 0x0f, 0x87, 0xf5, 0x03, 0x00,
    0x00, 0x49, 0x81, 0xf8, 0x90, 0x00, 0x00, 0x00, 0x0f, 0x85, 0xe8, 0x03, 0x00, 0x00, 0x80, 0x3d,
    0x43, 0xaf, 0xff, 0xff, 0x00, 0x0f, 0x84, 0xdb, 0x03, 0x00, 0x00, 0x48, 0x85, 0xf6, 0x0f, 0x84,
    0xb1, 0x03, 0x00, 0x00, 0x41, 0x80, 0xbe, 0x89, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x85, 0xc4, 0x03,
    0x00, 0x00, 0x48, 0x89, 0x75, 0xc0, 0x89, 0x55, 0xd4, 0x41, 0xc6, 0x86, 0x89, 0x00, 0x00, 0x00,
    0x01, 0x4d, 0x8d, 0x6e, 0x60, 0x49, 0x8b, 0x86, 0x80, 0x00, 0x00, 0x00, 0x45, 0x31, 0xc9, 0x4c,
    0x8d, 0x7d, 0xa8, 0x4c, 0x89, 0x65, 0xc8, 0x48, 0x89, 0x7d, 0xb8, 0x48, 0x8b, 0x0d, 0xee, 0xae,
    0xff, 0xff, 0x48, 0x39, 0xc8, 0x4c, 0x89, 0x4d, 0xb0, 0x0f, 0x85, 0xb7, 0x02, 0x00, 0x00, 0x41,
    0x0f, 0xb6, 0x86, 0x88, 0x00, 0x00, 0x00, 0x48, 0x89, 0xfb, 0xeb, 0x1d, 0xb9, 0x02, 0x00, 0x00,
    0x00, 0x4c, 0x89, 0xf7, 0x4c, 0x89, 0xee, 0x4c, 0x89, 0xfa, 0xe8, 0x81, 0x03, 0x00, 0x00, 0x41,
    0xc6, 0x86, 0x88, 0x00, 0x00, 0x00, 0x00, 0x31, 0xc0, 0xb9, 0x60, 0x00, 0x00, 0x00, 0x48, 0x29,
    0xc1, 0x4c, 0x39, 0xe1, 0x49, 0x0f, 0x43, 0xcc, 0x48, 0x85, 0xc9, 0x0f, 0x84, 0x14, 0x02, 0x00,
    0x00, 0x4c, 0x01, 0xf0, 0x48, 0x83, 0xf9, 0x08, 0x72, 0x1e, 0x48, 0x8d, 0x79, 0xf8, 0x89, 0xfa,
    0xf7, 0xd2, 0xf6, 0xc2, 0x18, 0x75, 0x1c, 0x48, 0x89, 0xde, 0x48, 0x89, 0xca, 0x48, 0x83, 0xff,
    0x18, 0x73, 0x4d, 0xe9, 0x97, 0x00, 0x00, 0x00, 0x48, 0x89, 0xca, 0x48, 0x89, 0xde, 0xe9, 0x8c,
    0x00, 0x00, 0x00, 0x89, 0xfe, 0xc1, 0xee, 0x03, 0xff, 0xc6, 0x83, 0xe6, 0x03, 0x31, 0xd2, 0x45,
    0x31, 0xc0, 0x4e, 0x8b, 0x0c, 0xc0, 0x4e, 0x89, 0x0c, 0xc3, 0x4a, 0xc7, 0x04, 0xc0, 0x00, 0x00,
    0x00, 0x00, 0x49, 0xff, 0xc0, 0x48, 0x83, 0xc2, 0xf8, 0x4c, 0x39, 0xc6, 0x75, 0xe4, 0x48, 0x89,
    0xde, 0x48, 0x29, 0xd6, 0x48, 0x29, 0xd0, 0x48, 0x01, 0xca, 0x48, 0x83, 0xff, 0x18, 0x72, 0x4f,
    0x48, 0x8b, 0x38, 0x48, 0x89, 0x3e, 0x48, 0xc7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8b, 0x78,
    0x08, 0x48, 0x89, 0x7e, 0x08, 0x48, 0xc7, 0x40, 0x08, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8b, 0x78,
    0x10, 0x48, 0x89, 0x7e, 0x10, 0x48, 0xc7, 0x40, 0x10, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8b, 0x78,
    0x18, 0x48, 0x89, 0x7e, 0x18, 0x48, 0xc7, 0x40, 0x18, 0x00, 0x00, 0x00, 0x00, 0x48, 0x83, 0xc6,
    0x20, 0x48, 0x83, 0xc0, 0x20, 0x48, 0x83, 0xc2, 0xe0, 0x48, 0x83, 0xfa, 0x07, 0x77, 0xb1, 0x48,
    0x83, 0xfa, 0x04, 0x0f, 0x82, 0x92, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x7a, 0xfc, 0x41, 0x89, 0xf8,
    0x41, 0xf7, 0xd0, 0x41, 0xf6, 0xc0, 0x0c, 0x74, 0x39, 0x41, 0x89, 0xf8, 0x41, 0xc1, 0xe8, 0x02,
    0x41, 0xff, 0xc0, 0x41, 0x83, 0xe0, 0x03, 0x45, 0x31, 0xc9, 0x45, 0x31, 0xd2, 0x46, 0x8b, 0x1c,
    0x90, 0x46, 0x89, 0x1c, 0x96, 0x42, 0xc7, 0x04, 0x90, 0x00, 0x00, 0x00, 0x00, 0x49, 0xff, 0xc2,
    0x49, 0x83, 0xc1, 0xfc, 0x4d, 0x39, 0xd0, 0x75, 0xe4, 0x4c, 0x29, 0xce, 0x4c, 0x29, 0xc8, 0x4c,
    0x01, 0xca, 0x48, 0x83, 0xff, 0x0c, 0x72, 0x43, 0x8b, 0x38, 0x89, 0x3e, 0xc7, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x8b, 0x78, 0x04, 0x89, 0x7e, 0x04, 0xc7, 0x40, 0x04, 0x00, 0x00, 0x00, 0x00, 0x8b,
    0x78, 0x08, 0x89, 0x7e, 0x08, 0xc7, 0x40, 0x08, 0x00, 0x00, 0x00, 0x00, 0x8b, 0x78, 0x0c, 0x89,
    0x7e, 0x0c, 0xc7, 0x40, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x48, 0x83, 0xc6, 0x10, 0x48, 0x83, 0xc0,
    0x10, 0x48, 0x83, 0xc2, 0xf0, 0x48, 0x83, 0xfa, 0x03, 0x77, 0xbd, 0x48, 0x83, 0xfa, 0x02, 0x0f,
    0x82, 0x96, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x7a, 0xfe, 0x41, 0x89, 0xf8, 0x41, 0xf7, 0xd0, 0x41,
    0xf6, 0xc0, 0x06, 0x74, 0x39, 0x41, 0x89, 0xf8, 0x41, 0xd1, 0xe8, 0x41, 0xff, 0xc0, 0x41, 0x83,
    0xe0, 0x03, 0x45, 0x31, 0xc9, 0x45, 0x31, 0xd2, 0x46, 0x0f, 0xb7, 0x1c, 0x50, 0x66, 0x46, 0x89,
    0x1c, 0x56, 0x66, 0x42, 0xc7, 0x04, 0x50, 0x00, 0x00, 0x49, 0xff, 0xc2, 0x49, 0x83, 0xc1, 0xfe,
    0x4d, 0x39, 0xd0, 0x75, 0xe3, 0x4c, 0x29, 0xce, 0x4c, 0x29, 0xc8, 0x4c, 0x01, 0xca, 0x48, 0x83,
    0xff, 0x06, 0x72, 0x47, 0x0f, 0xb7, 0x38, 0x66, 0x89, 0x3e, 0x66, 0xc7, 0x00, 0x00, 0x00, 0x0f,
    0xb7, 0x78, 0x02, 0x66, 0x89, 0x7e, 0x02, 0x66, 0xc7, 0x40, 0x02, 0x00, 0x00, 0x0f, 0xb7, 0x78,
    0x04, 0x66, 0x89, 0x7e, 0x04, 0x66, 0xc7, 0x40, 0x04, 0x00, 0x00, 0x0f, 0xb7, 0x78, 0x06, 0x66,
    0x89, 0x7e, 0x06, 0x66, 0xc7, 0x40, 0x06, 0x00, 0x00, 0x48, 0x83, 0xc6, 0x08, 0x48, 0x83, 0xc0,
    0x08, 0x48, 0x83, 0xc2, 0xf8, 0x48, 0x83, 0xfa, 0x01, 0x77, 0xb9, 0x48, 0x85, 0xd2, 0x74, 0x08,
    0x0f, 0xb6, 0x10, 0x88, 0x16, 0xc6, 0x00, 0x00, 0x41, 0x00, 0x8e, 0x88, 0x00, 0x00, 0x00, 0x48,
    0x01, 0xcb, 0x49, 0x29, 0xcc, 0x4d, 0x85, 0xe4, 0x74, 0x32, 0x49, 0x83, 0xfc, 0x40, 0x0f, 0x82,
    0xa8, 0xfd, 0xff, 0xff, 0x4c, 0x89, 0xe1, 0x48, 0xc1, 0xe9, 0x06, 0x48, 0x89, 0xdf, 0x4c, 0x89,
    0xee, 0x4c, 0x89, 0xfa, 0xe8, 0x27, 0x01, 0x00, 0x00, 0x4c, 0x89, 0xe0, 0x48, 0x83, 0xe0, 0xc0,
    0x48, 0x01, 0xc3, 0x41, 0x83, 0xe4, 0x3f, 0xe9, 0x80, 0xfd, 0xff, 0xff, 0x49, 0x8b, 0x86, 0x80,
    0x00, 0x00, 0x00, 0x48, 0x3b, 0x05, 0x46, 0xac, 0xff, 0xff, 0x74, 0x54, 0xf6, 0x45, 0xb0, 0x01,
    0x41, 0xb1, 0x01, 0x4c, 0x8b, 0x65, 0xc8, 0x48, 0x8b, 0x7d, 0xb8, 0x0f, 0x84, 0x3a, 0xfd, 0xff,
    0xff, 0xe9, 0xc1, 0x00, 0x00, 0x00, 0x49, 0x89, 0xf8, 0x49, 0x89, 0x8e, 0x80, 0x00, 0x00, 0x00,
    0xbe, 0x20, 0x00, 0x00, 0x00, 0xb8, 0x3e, 0x01, 0x00, 0x00, 0x4c, 0x89, 0xef, 0x31, 0xd2, 0x0f,
    0x05, 0x48, 0x83, 0xf8, 0x20, 0x0f, 0x85, 0x8e, 0x00, 0x00, 0x00, 0x41, 0xc6, 0x86, 0x88, 0x00,
    0x00, 0x00, 0x60, 0xb8, 0x60, 0x00, 0x00, 0x00, 0x4c, 0x89, 0xc7, 0xe9, 0x17, 0xfd, 0xff, 0xff,
    0x41, 0xc6, 0x86, 0x89, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8b, 0x45, 0xc8, 0x48, 0x83, 0xc4, 0x30,
    0x5b, 0x41, 0x5c, 0x41, 0x5d, 0x41, 0x5e, 0x41, 0x5f, 0x5d, 0xc3, 0x48, 0xb8, 0x90, 0x00, 0x00,
    0x00, 0x03, 0x00, 0x00, 0x00, 0x49, 0x89, 0x06, 0x41, 0xc7, 0x46, 0x08, 0x28, 0x00, 0x00, 0x00,
    0x49, 0xc7, 0x46, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x49, 0xc7, 0x46, 0x14, 0x00, 0x00, 0x00, 0x00,
    0x49, 0xc7, 0x46, 0x1c, 0x00, 0x00, 0x00, 0x00, 0x49, 0xc7, 0x46, 0x24, 0x00, 0x00, 0x00, 0x00,
    0x49, 0xc7, 0x46, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x49, 0xc7, 0x46, 0x34, 0x00, 0x00, 0x00, 0x00,
    0x41, 0xc7, 0x46, 0x3c, 0x00, 0x00, 0x00, 0x00, 0x31, 0xc0, 0xeb, 0xa0, 0x48, 0xc7, 0xc0, 0xf2,
    0xff, 0xff, 0xff, 0xeb, 0x97, 0x31, 0xc0, 0xeb, 0x93, 0x49, 0xc7, 0x86, 0x80, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x4c, 0x89, 0xc7, 0x41, 0xc6, 0x86, 0x89, 0x00, 0x00, 0x00, 0x00, 0x8b,
    0x55, 0xd4, 0x48, 0x8b, 0x75, 0xc0, 0xb8, 0x3e, 0x01, 0x00, 0x00, 0x0f, 0x05, 0xe9, 0x6a, 0xff,
    0xff, 0xff,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PatchPlan {
    pub(super) normal_branch_offset: usize,
    pub(super) normal_branch_expected: [u8; 5],
    pub(super) normal_branch_bytes: [u8; 5],
    pub(super) fallback_branch_offset: usize,
    pub(super) fallback_branch_expected: [u8; 5],
    pub(super) syscall_offset: usize,
    pub(super) syscall_word: [u8; 8],
    pub(super) common_epilogue_offset: usize,
}

fn invalid_layout(detail: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "unrecognized x86-64 __vdso_getrandom layout (expected the reviewed 1,122-byte function image): {}",
            detail.into()
        ),
    )
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub(super) fn complete_image_matches(image: &[u8]) -> bool {
    if image.len() != KNOWN_VDSO_IMAGE_BYTES
        || KNOWN_VDSO_IMAGE_HEX.len() != KNOWN_VDSO_IMAGE_BYTES * 2
    {
        return false;
    }
    image
        .iter()
        .zip(KNOWN_VDSO_IMAGE_HEX.as_bytes().as_chunks::<2>().0.iter())
        .all(|(observed, encoded)| {
            let Some(high) = hex_nibble(encoded[0]) else {
                return false;
            };
            let Some(low) = hex_nibble(encoded[1]) else {
                return false;
            };
            *observed == (high << 4 | low)
        })
}

#[cfg(test)]
pub(super) fn reviewed_image_for_test() -> Vec<u8> {
    KNOWN_VDSO_IMAGE_HEX
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|encoded| hex_nibble(encoded[0]).unwrap() << 4 | hex_nibble(encoded[1]).unwrap())
        .collect()
}

/// No reviewed whole image without a getrandom userspace route is admitted
/// yet. Future entries belong here only after their complete bytes prove
/// that neither a symbol nor a retained executable entry can reach one.
pub(super) fn complete_route_free_image_matches(_image: &[u8]) -> bool {
    false
}

fn rel32_target(bytes: &[u8], displacement_offset: usize, next_offset: usize) -> Option<usize> {
    let displacement = i32::from_le_bytes(
        bytes
            .get(displacement_offset..displacement_offset.checked_add(4)?)?
            .try_into()
            .ok()?,
    );
    let target = i64::try_from(next_offset)
        .ok()?
        .checked_add(i64::from(displacement))?;
    usize::try_from(target).ok()
}

fn rel8_target(bytes: &[u8], displacement_offset: usize, next_offset: usize) -> Option<usize> {
    let displacement = i8::from_le_bytes([*bytes.get(displacement_offset)?]);
    let target = i64::try_from(next_offset)
        .ok()?
        .checked_add(i64::from(displacement))?;
    usize::try_from(target).ok()
}

fn verify_instruction_targets(bytes: &[u8]) -> io::Result<()> {
    for branch in NORMAL_BRANCH_OFFSETS {
        if bytes.get(branch) != Some(&0x75)
            || rel8_target(bytes, branch + 1, branch + 2) != Some(NORMAL_PATH_OFFSET)
        {
            return Err(invalid_layout("normal-path query-gate target changed"));
        }
    }
    if bytes.get(QUERY_BRANCH_OFFSET..QUERY_BRANCH_OFFSET + 2) != Some(&[0x0f, 0x84]) {
        return Err(invalid_layout("query branch opcode changed"));
    }
    if rel32_target(bytes, QUERY_BRANCH_OFFSET + 2, QUERY_BRANCH_OFFSET + 6)
        != Some(QUERY_BODY_OFFSET)
    {
        return Err(invalid_layout("query branch target changed"));
    }
    if bytes.get(QUERY_RETURN_JUMP_OFFSET) != Some(&0xeb)
        || rel8_target(
            bytes,
            QUERY_RETURN_JUMP_OFFSET + 1,
            QUERY_RETURN_JUMP_OFFSET + 2,
        ) != Some(COMMON_EPILOGUE_OFFSET)
    {
        return Err(invalid_layout("query return target changed"));
    }
    if bytes.get(GENERAL_FALLBACK_OFFSET..GENERAL_SYSCALL_OFFSET + 2)
        != Some(&[0xb8, 0x3e, 0x01, 0x00, 0x00, 0x0f, 0x05])
    {
        return Err(invalid_layout("general getrandom syscall sequence changed"));
    }
    if bytes.get(GENERAL_RETURN_JUMP_OFFSET) != Some(&0xe9)
        || rel32_target(
            bytes,
            GENERAL_RETURN_JUMP_OFFSET + 1,
            GENERAL_RETURN_JUMP_OFFSET + 5,
        ) != Some(COMMON_EPILOGUE_OFFSET)
    {
        return Err(invalid_layout("general fallback return target changed"));
    }
    if rel32_target(&FORCE_GENERAL_FALLBACK, 1, FORCE_GENERAL_FALLBACK.len())
        != Some(GENERAL_FALLBACK_OFFSET - REDIRECT_OFFSET)
    {
        return Err(invalid_layout("replacement jump target is inconsistent"));
    }
    Ok(())
}

/// Return no patch when the symbol is absent. A present symbol must match the
/// entire reviewed function image before any byte is changed.
pub(super) fn plan(symbol: Option<&[u8]>) -> io::Result<Option<PatchPlan>> {
    let Some(symbol) = symbol else {
        return Ok(None);
    };
    if symbol != KNOWN_GETRANDOM {
        return Err(invalid_layout(format!(
            "function bytes or size changed (observed {} bytes, expected {})",
            symbol.len(),
            KNOWN_GETRANDOM.len()
        )));
    }
    verify_instruction_targets(symbol)?;
    Ok(Some(PatchPlan {
        normal_branch_offset: REDIRECT_OFFSET,
        normal_branch_expected: NORMAL_BRANCH_INSTRUCTION,
        normal_branch_bytes: FORCE_GENERAL_FALLBACK,
        fallback_branch_offset: GENERAL_FALLBACK_OFFSET,
        fallback_branch_expected: GENERAL_FALLBACK_INSTRUCTION,
        syscall_offset: GENERAL_SYSCALL_OFFSET,
        syscall_word: INTERNAL_SYSCALL_WORD,
        common_epilogue_offset: COMMON_EPILOGUE_OFFSET,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(plan: PatchPlan) -> Vec<u8> {
        let mut bytes = KNOWN_GETRANDOM.to_vec();
        assert_eq!(
            &bytes[plan.normal_branch_offset
                ..plan.normal_branch_offset + plan.normal_branch_expected.len()],
            plan.normal_branch_expected.as_slice(),
        );
        bytes
            [plan.normal_branch_offset..plan.normal_branch_offset + plan.normal_branch_bytes.len()]
            .copy_from_slice(&plan.normal_branch_bytes);
        bytes
    }

    const MARKER_FALLBACK_OFFSET: usize = 0x480;
    const MARKER_FALLBACK_RESULT: isize = -0x1234;

    fn rel32(from: usize, target: usize) -> [u8; 4] {
        let displacement =
            i64::try_from(target).unwrap() - i64::try_from(from.checked_add(5).unwrap()).unwrap();
        i32::try_from(displacement).unwrap().to_le_bytes()
    }

    fn apply_with_marker_fallback(plan: PatchPlan) -> Vec<u8> {
        let mut bytes = apply(plan);
        bytes.resize(MARKER_FALLBACK_OFFSET + 15, 0x90);
        assert_eq!(
            &bytes[plan.fallback_branch_offset..plan.fallback_branch_offset + 5],
            plan.fallback_branch_expected.as_slice(),
        );
        bytes[plan.fallback_branch_offset] = 0xe9;
        bytes[plan.fallback_branch_offset + 1..plan.fallback_branch_offset + 5]
            .copy_from_slice(&rel32(plan.fallback_branch_offset, MARKER_FALLBACK_OFFSET));
        bytes[MARKER_FALLBACK_OFFSET..MARKER_FALLBACK_OFFSET + 2].copy_from_slice(&[0x48, 0xb8]);
        bytes[MARKER_FALLBACK_OFFSET + 2..MARKER_FALLBACK_OFFSET + 10]
            .copy_from_slice(&(MARKER_FALLBACK_RESULT as i64).to_le_bytes());
        bytes[MARKER_FALLBACK_OFFSET + 10] = 0xe9;
        bytes[MARKER_FALLBACK_OFFSET + 11..MARKER_FALLBACK_OFFSET + 15].copy_from_slice(&rel32(
            MARKER_FALLBACK_OFFSET + 10,
            plan.common_epilogue_offset,
        ));
        bytes
    }

    type GetrandomFn =
        unsafe extern "C" fn(*mut libc::c_void, usize, u32, *mut libc::c_void, usize) -> isize;

    const PAGE_BYTES: usize = 0x1000;
    const SYNTHETIC_VDSO_BYTES: usize = 0x6000;
    const SYNTHETIC_FUNCTION_OFFSET: usize = 0x5050;
    const SYNTHETIC_GENERATION: u64 = 0x1122_3344_5566_7788;
    const OPAQUE_STATE_BYTES: usize = 0x90;

    struct SyntheticVdso {
        address: *mut libc::c_void,
        len: usize,
    }

    impl SyntheticVdso {
        fn new(bytes: &[u8]) -> Self {
            Self::new_with_ready(bytes, true)
        }

        fn new_with_ready(bytes: &[u8], ready: bool) -> Self {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            assert_eq!(page_size, PAGE_BYTES as libc::c_long);
            assert!(SYNTHETIC_FUNCTION_OFFSET + bytes.len() <= SYNTHETIC_VDSO_BYTES);
            let len = SYNTHETIC_VDSO_BYTES;
            let address = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(address, libc::MAP_FAILED);
            unsafe {
                core::ptr::write_unaligned(address.cast::<u64>(), SYNTHETIC_GENERATION);
                core::ptr::write(address.cast::<u8>().add(8), u8::from(ready));
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    address.cast::<u8>().add(SYNTHETIC_FUNCTION_OFFSET),
                    bytes.len(),
                );
            }
            for (offset, protection) in [
                (0, libc::PROT_READ),
                (PAGE_BYTES, libc::PROT_NONE),
                (5 * PAGE_BYTES, libc::PROT_READ | libc::PROT_EXEC),
            ] {
                let protect_len = if offset == PAGE_BYTES {
                    4 * PAGE_BYTES
                } else {
                    PAGE_BYTES
                };
                let result = unsafe {
                    libc::mprotect(
                        address.cast::<u8>().add(offset).cast(),
                        protect_len,
                        protection,
                    )
                };
                if result == 0 {
                    continue;
                }
                unsafe {
                    libc::munmap(address, len);
                }
                panic!("could not protect synthetic vDSO mapping");
            }
            Self { address, len }
        }

        fn function(&self) -> GetrandomFn {
            let address = unsafe {
                self.address
                    .cast::<u8>()
                    .add(SYNTHETIC_FUNCTION_OFFSET)
                    .cast::<libc::c_void>()
            };
            unsafe { core::mem::transmute::<*mut libc::c_void, GetrandomFn>(address) }
        }
    }

    impl Drop for SyntheticVdso {
        fn drop(&mut self) {
            let result = unsafe { libc::munmap(self.address, self.len) };
            assert_eq!(result, 0);
        }
    }

    struct WritableState {
        address: *mut u8,
    }

    impl WritableState {
        fn new() -> Self {
            let address = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    2 * PAGE_BYTES,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(address, libc::MAP_FAILED);
            Self {
                address: address.cast(),
            }
        }

        fn pointer(&self, offset: usize) -> *mut libc::c_void {
            assert!(offset + OPAQUE_STATE_BYTES <= 2 * PAGE_BYTES);
            unsafe { self.address.add(offset).cast() }
        }

        fn prepare(&self, offset: usize) -> [u8; OPAQUE_STATE_BYTES] {
            let mut state = [0u8; OPAQUE_STATE_BYTES];
            for (index, byte) in state[..0x60].iter_mut().enumerate() {
                *byte = u8::try_from(index + 1).unwrap();
            }
            state[0x80..0x88].copy_from_slice(&SYNTHETIC_GENERATION.to_ne_bytes());
            state[0x88] = 0;
            state[0x89] = 0;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    state.as_ptr(),
                    self.pointer(offset).cast(),
                    state.len(),
                );
            }
            state
        }

        fn read(&self, offset: usize) -> [u8; OPAQUE_STATE_BYTES] {
            let mut state = [0u8; OPAQUE_STATE_BYTES];
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.pointer(offset).cast(),
                    state.as_mut_ptr(),
                    state.len(),
                );
            }
            state
        }
    }

    impl Drop for WritableState {
        fn drop(&mut self) {
            let result = unsafe { libc::munmap(self.address.cast(), 2 * PAGE_BYTES) };
            assert_eq!(result, 0);
        }
    }

    const SYNTHETIC_ELF_BYTES: usize = 0x2000;
    const SYNTHETIC_DYNAMIC_OFFSET: usize = 0x180;
    const SYNTHETIC_DYNSTR_OFFSET: usize = 0x300;
    const SYNTHETIC_DYNSYM_OFFSET: usize = 0x400;
    const SYNTHETIC_HASH_OFFSET: usize = 0x500;
    const SYNTHETIC_SECTION_HEADERS_OFFSET: usize = 0x600;
    const SYNTHETIC_CODE_OFFSET: usize = 0x800;
    const SYNTHETIC_GUARD_OFFSET: usize = 0x900;
    const SYNTHETIC_ELF_FUNCTION_OFFSET: usize = 0x1000;
    const SYNTHETIC_EXECUTABLE_END: usize = 0x1500;
    const SYNTHETIC_ALTERNATE_STREAM: [u8; 10] =
        [0x48, 0xb8, 0xe9, 0x93, 0x08, 0x00, 0x00, 0x90, 0x90, 0x90];

    #[derive(Clone, Copy)]
    struct SyntheticSymbol {
        name: &'static str,
        value: u64,
        size: u64,
        info: u8,
        other: u8,
        section: u16,
    }

    impl SyntheticSymbol {
        fn canonical() -> Self {
            Self::function("__vdso_getrandom", goblin::elf::sym::STB_GLOBAL)
        }

        fn alias() -> Self {
            Self::function("getrandom", goblin::elf::sym::STB_WEAK)
        }

        fn function(name: &'static str, binding: u8) -> Self {
            Self {
                name,
                value: SYNTHETIC_ELF_FUNCTION_OFFSET as u64,
                size: KNOWN_GETRANDOM.len() as u64,
                info: binding << 4 | goblin::elf::sym::STT_FUNC,
                other: goblin::elf::sym::STV_DEFAULT,
                section: 1,
            }
        }
    }

    fn put_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn synthetic_elf(symbols: &[SyntheticSymbol], load_vaddr: u64) -> Vec<u8> {
        let mut image = vec![0u8; SYNTHETIC_ELF_BYTES];
        let image_len = image.len() as u64;
        image[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        put_u16(&mut image, 16, goblin::elf::header::ET_DYN);
        put_u16(&mut image, 18, goblin::elf::header::EM_X86_64);
        put_u32(&mut image, 20, 1);
        put_u64(&mut image, 32, 0x40);
        put_u64(&mut image, 40, SYNTHETIC_SECTION_HEADERS_OFFSET as u64);
        put_u16(&mut image, 52, 0x40);
        put_u16(&mut image, 54, 0x38);
        put_u16(&mut image, 56, 2);
        put_u16(&mut image, 58, 0x40);
        put_u16(&mut image, 60, 2);

        put_u32(&mut image, 0x40, goblin::elf::program_header::PT_LOAD);
        put_u32(
            &mut image,
            0x44,
            goblin::elf::program_header::PF_R | goblin::elf::program_header::PF_X,
        );
        put_u64(&mut image, 0x50, load_vaddr);
        put_u64(&mut image, 0x60, image_len);
        put_u64(&mut image, 0x68, image_len);
        put_u64(&mut image, 0x70, PAGE_BYTES as u64);

        let dynamic_header = 0x40 + 0x38;
        put_u32(
            &mut image,
            dynamic_header,
            goblin::elf::program_header::PT_DYNAMIC,
        );
        put_u32(
            &mut image,
            dynamic_header + 4,
            goblin::elf::program_header::PF_R,
        );
        put_u64(
            &mut image,
            dynamic_header + 8,
            SYNTHETIC_DYNAMIC_OFFSET as u64,
        );
        put_u64(
            &mut image,
            dynamic_header + 16,
            SYNTHETIC_DYNAMIC_OFFSET as u64,
        );
        put_u64(&mut image, dynamic_header + 32, 6 * 16);
        put_u64(&mut image, dynamic_header + 40, 6 * 16);
        put_u64(&mut image, dynamic_header + 48, 8);

        let executable_section = SYNTHETIC_SECTION_HEADERS_OFFSET + 0x40;
        put_u32(
            &mut image,
            executable_section + 4,
            goblin::elf::section_header::SHT_PROGBITS,
        );
        put_u64(
            &mut image,
            executable_section + 8,
            u64::from(
                goblin::elf::section_header::SHF_ALLOC | goblin::elf::section_header::SHF_EXECINSTR,
            ),
        );
        put_u64(
            &mut image,
            executable_section + 16,
            SYNTHETIC_CODE_OFFSET as u64,
        );
        put_u64(
            &mut image,
            executable_section + 24,
            SYNTHETIC_CODE_OFFSET as u64,
        );
        put_u64(
            &mut image,
            executable_section + 32,
            (SYNTHETIC_EXECUTABLE_END - SYNTHETIC_CODE_OFFSET) as u64,
        );
        put_u64(&mut image, executable_section + 48, 16);

        let mut strings = vec![0u8];
        let mut name_offsets = Vec::new();
        for symbol in symbols {
            name_offsets.push(strings.len());
            strings.extend_from_slice(symbol.name.as_bytes());
            strings.push(0);
        }
        image[SYNTHETIC_DYNSTR_OFFSET..SYNTHETIC_DYNSTR_OFFSET + strings.len()]
            .copy_from_slice(&strings);

        for (index, (symbol, name_offset)) in symbols.iter().zip(name_offsets).enumerate() {
            let offset = SYNTHETIC_DYNSYM_OFFSET + (index + 1) * 24;
            put_u32(&mut image, offset, name_offset as u32);
            image[offset + 4] = symbol.info;
            image[offset + 5] = symbol.other;
            put_u16(&mut image, offset + 6, symbol.section);
            put_u64(&mut image, offset + 8, symbol.value);
            put_u64(&mut image, offset + 16, symbol.size);
        }

        let dynamic_values = [
            (
                goblin::elf::dynamic::DT_STRTAB,
                SYNTHETIC_DYNSTR_OFFSET as u64,
            ),
            (goblin::elf::dynamic::DT_STRSZ, strings.len() as u64),
            (
                goblin::elf::dynamic::DT_SYMTAB,
                SYNTHETIC_DYNSYM_OFFSET as u64,
            ),
            (goblin::elf::dynamic::DT_SYMENT, 24),
            (goblin::elf::dynamic::DT_HASH, SYNTHETIC_HASH_OFFSET as u64),
            (goblin::elf::dynamic::DT_NULL, 0),
        ];
        for (index, (tag, value)) in dynamic_values.into_iter().enumerate() {
            let offset = SYNTHETIC_DYNAMIC_OFFSET + index * 16;
            put_u64(&mut image, offset, tag);
            put_u64(&mut image, offset + 8, value);
        }

        put_u32(&mut image, SYNTHETIC_HASH_OFFSET, 1);
        put_u32(
            &mut image,
            SYNTHETIC_HASH_OFFSET + 4,
            (symbols.len() + 1) as u32,
        );
        put_u32(
            &mut image,
            SYNTHETIC_HASH_OFFSET + 8,
            u32::from(!symbols.is_empty()),
        );
        // The reviewed raw fallback is eight bytes beginning at function
        // offset 0x45b. Its final NOP is the byte immediately after the
        // 0x462-byte symbol, so the synthetic executable image must retain
        // that post-symbol byte as well.
        image[SYNTHETIC_CODE_OFFSET..SYNTHETIC_EXECUTABLE_END].fill(0x90);
        image[SYNTHETIC_GUARD_OFFSET..SYNTHETIC_GUARD_OFFSET + SGX_GUARD_SOURCE.len()]
            .copy_from_slice(&SGX_GUARD_SOURCE);
        image[SYNTHETIC_ELF_FUNCTION_OFFSET..SYNTHETIC_ELF_FUNCTION_OFFSET + KNOWN_GETRANDOM.len()]
            .copy_from_slice(KNOWN_GETRANDOM);
        image
    }

    fn parsed_plan(image: &[u8]) -> Result<Option<(usize, PatchPlan)>, reverie::Error> {
        super::super::liteinst_getrandom_symbol_layout(image, SYNTHETIC_GUARD_OFFSET as u64)
    }

    fn known_vdso_image() -> Vec<u8> {
        KNOWN_VDSO_IMAGE_HEX
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|encoded| hex_nibble(encoded[0]).unwrap() << 4 | hex_nibble(encoded[1]).unwrap())
            .collect()
    }

    #[test]
    fn production_admission_binds_every_complete_image_byte() {
        let absent = synthetic_elf(&[], 0);
        assert!(!complete_image_matches(&absent));
        let error = super::super::liteinst_getrandom_symbol(&absent).unwrap_err();
        assert!(
            matches!(
                &error,
                reverie::Error::Io(error)
                    if error.kind() == std::io::ErrorKind::InvalidData
            ),
            "absent unreviewed image reached the wrong refusal: {error}"
        );
        assert!(
            error.to_string().contains(
                "lacks both getrandom ABI names and is not an exact reviewed route-free image"
            ),
            "absent unreviewed image reached the wrong refusal: {error}"
        );

        let image = known_vdso_image();
        assert_eq!(image.len(), KNOWN_VDSO_IMAGE_BYTES);
        assert!(complete_image_matches(&image));
        assert_eq!(
            super::super::liteinst_getrandom_symbol(&image)
                .unwrap()
                .unwrap()
                .0,
            0x1050,
        );
        for offset in [
            0,
            SYNTHETIC_CODE_OFFSET,
            SGX_INDIRECT_CALL_OFFSET,
            image.len() - 1,
        ] {
            let mut changed = image.clone();
            changed[offset] ^= 1;
            assert!(!complete_image_matches(&changed));
            let error = super::super::liteinst_getrandom_symbol(&changed).unwrap_err();
            assert!(
                matches!(
                    &error,
                    reverie::Error::Io(error)
                        if error.kind() == std::io::ErrorKind::InvalidData
                ),
                "changed present image reached the wrong refusal: {error}"
            );
        }
    }

    #[test]
    fn synthetic_elf_binds_every_public_getrandom_alias() {
        assert_eq!(parsed_plan(&synthetic_elf(&[], 0)).unwrap(), None);

        let expected = Some((
            SYNTHETIC_ELF_FUNCTION_OFFSET,
            plan(Some(KNOWN_GETRANDOM)).unwrap().unwrap(),
        ));
        assert_eq!(
            parsed_plan(&synthetic_elf(&[SyntheticSymbol::canonical()], 0)).unwrap(),
            expected,
        );
        assert_eq!(
            parsed_plan(&synthetic_elf(&[SyntheticSymbol::alias()], 0)).unwrap(),
            expected,
        );
        assert_eq!(
            parsed_plan(&synthetic_elf(
                &[SyntheticSymbol::canonical(), SyntheticSymbol::alias()],
                0,
            ))
            .unwrap(),
            expected,
        );

        let mut interior_alias = SyntheticSymbol::alias();
        interior_alias.value += 1;
        interior_alias.size -= 1;
        assert!(
            parsed_plan(&synthetic_elf(
                &[SyntheticSymbol::canonical(), interior_alias],
                0,
            ))
            .is_err()
        );

        let mut third_entry =
            SyntheticSymbol::function("third_getrandom_entry", goblin::elf::sym::STB_GLOBAL);
        third_entry.value += REDIRECT_OFFSET as u64 + 1;
        third_entry.size = 4;
        assert!(
            parsed_plan(&synthetic_elf(
                &[SyntheticSymbol::canonical(), third_entry],
                0,
            ))
            .is_err()
        );

        // The stopped redirect does not rewrite the original syscall or its
        // following jump. An independent entry at that untouched jump is
        // therefore accepted rather than hidden by an overbroad refusal.
        let mut untouched_tail_entry =
            SyntheticSymbol::function("third_getrandom_tail_entry", goblin::elf::sym::STB_GLOBAL);
        untouched_tail_entry.value += GENERAL_RETURN_JUMP_OFFSET as u64;
        untouched_tail_entry.size = 5;
        assert_eq!(
            parsed_plan(&synthetic_elf(
                &[SyntheticSymbol::canonical(), untouched_tail_entry],
                0,
            ))
            .unwrap(),
            expected,
        );
    }

    #[test]
    fn synthetic_elf_refuses_outside_direct_entries_into_changed_instructions() {
        let canonical = [SyntheticSymbol::canonical()];
        let mut rewritten_interiors = Vec::new();
        rewritten_interiors
            .extend((1..5).map(|offset| SYNTHETIC_ELF_FUNCTION_OFFSET + REDIRECT_OFFSET + offset));
        rewritten_interiors.extend(
            (1..5).map(|offset| SYNTHETIC_ELF_FUNCTION_OFFSET + GENERAL_FALLBACK_OFFSET + offset),
        );
        rewritten_interiors
            .extend((1..SGX_GUARD_DISPLACED_LEN).map(|offset| SYNTHETIC_GUARD_OFFSET + offset));
        for target in rewritten_interiors {
            let mut image = synthetic_elf(&canonical, 0);
            let displacement = i32::try_from(target)
                .unwrap()
                .checked_sub(i32::try_from(SYNTHETIC_CODE_OFFSET + 5).unwrap())
                .unwrap();
            image[SYNTHETIC_CODE_OFFSET] = 0xe9;
            image[SYNTHETIC_CODE_OFFSET + 1..SYNTHETIC_CODE_OFFSET + 5]
                .copy_from_slice(&displacement.to_le_bytes());
            assert!(parsed_plan(&image).is_err());
        }

        // A direct transfer to the original, untouched return jump is not
        // an entry into either replaced instruction.
        let mut image = synthetic_elf(&canonical, 0);
        let displacement =
            i32::try_from(SYNTHETIC_ELF_FUNCTION_OFFSET + GENERAL_RETURN_JUMP_OFFSET)
                .unwrap()
                .checked_sub(i32::try_from(SYNTHETIC_CODE_OFFSET + 5).unwrap())
                .unwrap();
        image[SYNTHETIC_CODE_OFFSET] = 0xe9;
        image[SYNTHETIC_CODE_OFFSET + 1..SYNTHETIC_CODE_OFFSET + 5]
            .copy_from_slice(&displacement.to_le_bytes());
        assert!(parsed_plan(&image).unwrap().is_some());
    }

    #[test]
    fn synthetic_elf_refuses_ifunc_and_extra_computed_targets_for_the_actual_reason() {
        let mut ifunc = SyntheticSymbol::function("computed_ifunc", goblin::elf::sym::STB_GLOBAL);
        ifunc.value = (SYNTHETIC_CODE_OFFSET + 0x40) as u64;
        ifunc.size = 1;
        ifunc.info = goblin::elf::sym::STB_GLOBAL << 4 | goblin::elf::sym::STT_GNU_IFUNC;
        let error =
            parsed_plan(&synthetic_elf(&[SyntheticSymbol::canonical(), ifunc], 0)).unwrap_err();
        assert!(
            error.to_string().contains("defined GNU IFUNC"),
            "unexpected IFUNC refusal: {error}",
        );

        for (name, bytes) in [
            ("register-indirect call", [0xff, 0xd1]),
            ("register-indirect jump", [0xff, 0xe0]),
            ("memory-indirect call", [0xff, 0x10]),
            ("memory-indirect jump", [0xff, 0x20]),
        ] {
            let mut image = synthetic_elf(&[SyntheticSymbol::canonical()], 0);
            image[SYNTHETIC_CODE_OFFSET + 0x40..SYNTHETIC_CODE_OFFSET + 0x42]
                .copy_from_slice(&bytes);
            let error = parsed_plan(&image).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unsupported indirect call or jump"),
                "unexpected {name} refusal: {error}",
            );
        }
    }

    #[test]
    fn synthetic_elf_refuses_exported_alternate_instruction_streams() {
        for symbol_type in [goblin::elf::sym::STT_FUNC, goblin::elf::sym::STT_NOTYPE] {
            for section in [1, goblin::elf::section_header::SHN_ABS as u16] {
                let mut alternate = SyntheticSymbol::function(
                    "third_alternate_entry",
                    goblin::elf::sym::STB_GLOBAL,
                );
                alternate.value = (SYNTHETIC_CODE_OFFSET + 2) as u64;
                alternate.size = 5;
                alternate.info = goblin::elf::sym::STB_GLOBAL << 4 | symbol_type;
                alternate.section = section;
                let mut image = synthetic_elf(&[SyntheticSymbol::canonical(), alternate], 0);
                image[SYNTHETIC_CODE_OFFSET
                    ..SYNTHETIC_CODE_OFFSET + SYNTHETIC_ALTERNATE_STREAM.len()]
                    .copy_from_slice(&SYNTHETIC_ALTERNATE_STREAM);

                assert_eq!(
                    &image[SYNTHETIC_ELF_FUNCTION_OFFSET
                        ..SYNTHETIC_ELF_FUNCTION_OFFSET + KNOWN_GETRANDOM.len()],
                    KNOWN_GETRANDOM,
                );
                let alternate_next = SYNTHETIC_CODE_OFFSET + 7;
                let displacement = i32::from_le_bytes(
                    image[SYNTHETIC_CODE_OFFSET + 3..SYNTHETIC_CODE_OFFSET + 7]
                        .try_into()
                        .unwrap(),
                );
                assert_eq!(
                    i64::try_from(alternate_next).unwrap() + i64::from(displacement),
                    i64::try_from(SYNTHETIC_ELF_FUNCTION_OFFSET + REDIRECT_OFFSET + 1).unwrap(),
                );

                let error = parsed_plan(&image).unwrap_err();
                assert!(
                    error.to_string().contains(
                        "exported callable symbol is not a canonical decoded instruction head"
                    ),
                    "unexpected refusal for symbol type {symbol_type}, section {section}: {error}",
                );
            }
        }
    }

    #[test]
    fn synthetic_elf_refuses_overlapping_executable_decode_origins() {
        let mut alternate =
            SyntheticSymbol::function("third_alternate_entry", goblin::elf::sym::STB_GLOBAL);
        alternate.value = (SYNTHETIC_CODE_OFFSET + 2) as u64;
        alternate.size = 5;
        alternate.section = 2;
        let mut image = synthetic_elf(&[SyntheticSymbol::canonical(), alternate], 0);
        image[SYNTHETIC_CODE_OFFSET..SYNTHETIC_CODE_OFFSET + SYNTHETIC_ALTERNATE_STREAM.len()]
            .copy_from_slice(&SYNTHETIC_ALTERNATE_STREAM);

        put_u16(&mut image, 60, 3);
        let first = SYNTHETIC_SECTION_HEADERS_OFFSET + 0x40;
        let overlapping = SYNTHETIC_SECTION_HEADERS_OFFSET + 0x80;
        let first_header = image[first..first + 0x40].to_vec();
        image[overlapping..overlapping + 0x40].copy_from_slice(&first_header);
        let overlapping_start = (SYNTHETIC_CODE_OFFSET + 2) as u64;
        let executable_end = (SYNTHETIC_ELF_FUNCTION_OFFSET + KNOWN_GETRANDOM.len()) as u64;
        put_u64(&mut image, overlapping + 16, overlapping_start);
        put_u64(&mut image, overlapping + 24, overlapping_start);
        put_u64(
            &mut image,
            overlapping + 32,
            executable_end - overlapping_start,
        );

        let error = parsed_plan(&image).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("current vDSO has overlapping executable sections"),
            "unexpected overlapping-section refusal: {error}",
        );
    }

    #[test]
    fn synthetic_elf_refuses_invalid_symbols_and_load_geometry() {
        let mut non_function = SyntheticSymbol::canonical();
        non_function.info = goblin::elf::sym::STB_GLOBAL << 4 | goblin::elf::sym::STT_OBJECT;
        let mut undefined = SyntheticSymbol::canonical();
        undefined.section = goblin::elf::section_header::SHN_UNDEF as u16;
        let mut out_of_range = SyntheticSymbol::canonical();
        out_of_range.size = SYNTHETIC_ELF_BYTES as u64;
        let mut wrong_alias_binding = SyntheticSymbol::alias();
        wrong_alias_binding.info = goblin::elf::sym::STB_GLOBAL << 4 | goblin::elf::sym::STT_FUNC;

        for symbols in [
            vec![non_function],
            vec![undefined],
            vec![out_of_range],
            vec![wrong_alias_binding],
            vec![SyntheticSymbol::canonical(), SyntheticSymbol::canonical()],
            vec![SyntheticSymbol::alias(), SyntheticSymbol::alias()],
        ] {
            assert!(parsed_plan(&synthetic_elf(&symbols, 0)).is_err());
        }

        let canonical = [SyntheticSymbol::canonical()];
        let mut invalid_loads = Vec::new();
        invalid_loads.push(("nonzero load address", synthetic_elf(&canonical, 0x1000)));
        let mut nonzero_file_offset = synthetic_elf(&canonical, 0);
        put_u64(&mut nonzero_file_offset, 0x48, 1);
        invalid_loads.push(("nonzero file offset", nonzero_file_offset));
        let mut unequal_sizes = synthetic_elf(&canonical, 0);
        put_u64(&mut unequal_sizes, 0x68, (SYNTHETIC_ELF_BYTES - 1) as u64);
        invalid_loads.push(("unequal file and memory sizes", unequal_sizes));
        let mut wrong_alignment = synthetic_elf(&canonical, 0);
        put_u64(&mut wrong_alignment, 0x70, 2 * PAGE_BYTES as u64);
        invalid_loads.push(("wrong load alignment", wrong_alignment));
        let mut writable_executable = synthetic_elf(&canonical, 0);
        put_u32(
            &mut writable_executable,
            0x44,
            goblin::elf::program_header::PF_R
                | goblin::elf::program_header::PF_W
                | goblin::elf::program_header::PF_X,
        );
        invalid_loads.push(("writable executable load", writable_executable));
        let mut two_executable_loads = synthetic_elf(&canonical, 0);
        put_u16(&mut two_executable_loads, 56, 3);
        let load = two_executable_loads[0x40..0x78].to_vec();
        two_executable_loads[0xb0..0xe8].copy_from_slice(&load);
        invalid_loads.push(("two executable loads", two_executable_loads));
        let mut tight_executable_load = synthetic_elf(&canonical, 0);
        let tight_end = (SYNTHETIC_ELF_FUNCTION_OFFSET + KNOWN_GETRANDOM.len()) as u64;
        put_u64(&mut tight_executable_load, 0x60, tight_end);
        put_u64(&mut tight_executable_load, 0x68, tight_end);
        invalid_loads.push(("raw syscall word crosses tight load", tight_executable_load));

        for (case, image) in invalid_loads {
            assert!(
                parsed_plan(&image).is_err(),
                "invalid load geometry was accepted: {case}"
            );
        }
    }

    #[test]
    fn recognized_present_and_absent_symbols_have_distinct_results() {
        assert_eq!(plan(None).unwrap(), None);
        assert_eq!(
            plan(Some(KNOWN_GETRANDOM)).unwrap(),
            Some(PatchPlan {
                normal_branch_offset: 0x99,
                normal_branch_expected: [0x41, 0xc6, 0x86, 0x89, 0x00],
                normal_branch_bytes: [0xe9, 0xb8, 0x03, 0x00, 0x00],
                fallback_branch_offset: 0x456,
                fallback_branch_expected: [0xb8, 0x3e, 0x01, 0x00, 0x00],
                syscall_offset: 0x45b,
                syscall_word: [0x0f, 0x05, 0xe9, 0x6a, 0xff, 0xff, 0xff, 0x90],
                common_epilogue_offset: 0x3cc,
            })
        );
    }

    #[test]
    fn query_gate_and_query_body_remain_byte_identical() {
        let patched = apply(plan(Some(KNOWN_GETRANDOM)).unwrap().unwrap());

        assert_eq!(
            &patched[..REDIRECT_OFFSET],
            &KNOWN_GETRANDOM[..REDIRECT_OFFSET]
        );
        assert_eq!(
            rel32_target(&patched, QUERY_BRANCH_OFFSET + 2, QUERY_BRANCH_OFFSET + 6,),
            Some(QUERY_BODY_OFFSET)
        );
        assert_eq!(
            &patched[QUERY_BODY_OFFSET..],
            &KNOWN_GETRANDOM[QUERY_BODY_OFFSET..],
        );
    }

    #[test]
    fn copied_machine_code_preserves_query_and_routes_normal_work_to_the_fallback() {
        let plan = plan(Some(KNOWN_GETRANDOM)).unwrap().unwrap();
        let baseline_mapping = SyntheticVdso::new(KNOWN_GETRANDOM);
        let real_fallback_bytes = apply(plan);
        let real_fallback_mapping = SyntheticVdso::new(&real_fallback_bytes);
        let fallback_bytes = apply_with_marker_fallback(plan);
        let patched_mapping = SyntheticVdso::new(&fallback_bytes);
        let not_ready_mapping = SyntheticVdso::new_with_ready(&fallback_bytes, false);
        let baseline = baseline_mapping.function();
        let real_fallback = real_fallback_mapping.function();
        let patched = patched_mapping.function();
        let not_ready = not_ready_mapping.function();

        let mut baseline_params = [0xa5; 64];
        let mut patched_params = baseline_params;
        let baseline_result = unsafe {
            baseline(
                core::ptr::null_mut(),
                0,
                0,
                baseline_params.as_mut_ptr().cast(),
                usize::MAX,
            )
        };
        let patched_result = unsafe {
            patched(
                core::ptr::null_mut(),
                0,
                0,
                patched_params.as_mut_ptr().cast(),
                usize::MAX,
            )
        };
        assert_eq!(baseline_result, 0);
        assert_eq!(patched_result, baseline_result);
        assert_eq!(patched_params, baseline_params);
        assert_eq!(
            u32::from_le_bytes(patched_params[0..4].try_into().unwrap()),
            0x90
        );
        assert_eq!(
            u32::from_le_bytes(patched_params[4..8].try_into().unwrap()),
            0x03
        );
        assert_eq!(
            u32::from_le_bytes(patched_params[8..12].try_into().unwrap()),
            0x28
        );
        assert!(patched_params[12..].iter().all(|byte| *byte == 0));

        let baseline_state = WritableState::new();
        let patched_state = WritableState::new();
        let baseline_before = baseline_state.prepare(0);
        let patched_before = patched_state.prepare(0);
        let mut baseline_entropy = [0; 16];
        let mut patched_entropy = [0; 16];
        assert_eq!(
            unsafe {
                baseline(
                    baseline_entropy.as_mut_ptr().cast(),
                    baseline_entropy.len(),
                    0,
                    baseline_state.pointer(0),
                    OPAQUE_STATE_BYTES,
                )
            },
            baseline_entropy.len() as isize,
        );
        assert_eq!(baseline_entropy, baseline_before[..baseline_entropy.len()]);
        let baseline_after = baseline_state.read(0);
        assert_ne!(baseline_after, baseline_before);
        assert!(
            baseline_after[..baseline_entropy.len()]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(baseline_after[0x88], baseline_entropy.len() as u8);
        assert_eq!(baseline_after[0x89], 0);

        assert_eq!(
            unsafe {
                patched(
                    patched_entropy.as_mut_ptr().cast(),
                    patched_entropy.len(),
                    0,
                    patched_state.pointer(0),
                    OPAQUE_STATE_BYTES,
                )
            },
            MARKER_FALLBACK_RESULT,
        );
        assert_eq!(patched_entropy, [0; 16]);
        assert_eq!(patched_state.read(0), patched_before);

        let boundary_offset = 0xf71;
        let boundary_state = WritableState::new();
        let boundary_before = boundary_state.prepare(boundary_offset);
        let mut boundary_output = [0xa5];
        for function in [baseline, patched] {
            assert_eq!(
                unsafe {
                    function(
                        boundary_output.as_mut_ptr().cast(),
                        boundary_output.len(),
                        0,
                        boundary_state.pointer(boundary_offset),
                        OPAQUE_STATE_BYTES,
                    )
                },
                -(libc::EFAULT as isize),
            );
            assert_eq!(boundary_output, [0xa5]);
            assert_eq!(boundary_state.read(boundary_offset), boundary_before);
        }

        let mut entropy = [0; 16];
        assert_eq!(
            unsafe {
                patched(
                    entropy.as_mut_ptr().cast(),
                    entropy.len(),
                    0,
                    core::ptr::null_mut(),
                    0,
                )
            },
            MARKER_FALLBACK_RESULT,
        );
        assert_eq!(
            unsafe {
                patched(
                    core::ptr::null_mut(),
                    0,
                    libc::GRND_NONBLOCK,
                    core::ptr::null_mut(),
                    0,
                )
            },
            MARKER_FALLBACK_RESULT,
        );
        assert_eq!(
            unsafe {
                real_fallback(
                    core::ptr::null_mut(),
                    0,
                    libc::GRND_NONBLOCK,
                    core::ptr::null_mut(),
                    0,
                )
            },
            0,
        );
        assert_eq!(
            unsafe {
                patched(
                    entropy.as_mut_ptr().cast(),
                    1,
                    0x8000_0000,
                    core::ptr::null_mut(),
                    0,
                )
            },
            MARKER_FALLBACK_RESULT,
        );
        assert_eq!(
            unsafe { patched(core::ptr::null_mut(), 1, 0, core::ptr::null_mut(), 0,) },
            MARKER_FALLBACK_RESULT,
        );

        let in_use_state = WritableState::new();
        in_use_state.prepare(0);
        unsafe {
            in_use_state.pointer(0).cast::<u8>().add(0x89).write(1);
        }
        let in_use_before = in_use_state.read(0);
        assert_eq!(
            unsafe {
                patched(
                    entropy.as_mut_ptr().cast(),
                    1,
                    0,
                    in_use_state.pointer(0),
                    OPAQUE_STATE_BYTES,
                )
            },
            MARKER_FALLBACK_RESULT,
        );
        assert_eq!(in_use_state.read(0), in_use_before);

        let not_ready_state = WritableState::new();
        let not_ready_before = not_ready_state.prepare(0);
        assert_eq!(
            unsafe {
                not_ready(
                    entropy.as_mut_ptr().cast(),
                    1,
                    0,
                    not_ready_state.pointer(0),
                    OPAQUE_STATE_BYTES,
                )
            },
            MARKER_FALLBACK_RESULT,
        );
        assert_eq!(not_ready_state.read(0), not_ready_before);
    }

    #[test]
    fn normal_branch_targets_the_existing_fallback_without_touching_its_syscall() {
        let plan = plan(Some(KNOWN_GETRANDOM)).unwrap().unwrap();
        let patched = apply_with_marker_fallback(plan);

        for branch in NORMAL_BRANCH_OFFSETS {
            assert_eq!(
                rel8_target(&patched, branch + 1, branch + 2),
                Some(NORMAL_PATH_OFFSET),
            );
        }
        assert_eq!(
            rel32_target(&patched, REDIRECT_OFFSET + 1, REDIRECT_OFFSET + 5),
            Some(GENERAL_FALLBACK_OFFSET),
        );
        assert_eq!(
            rel32_target(
                &patched,
                GENERAL_FALLBACK_OFFSET + 1,
                GENERAL_FALLBACK_OFFSET + 5,
            ),
            Some(MARKER_FALLBACK_OFFSET),
        );
        assert_eq!(
            &patched[GENERAL_SYSCALL_OFFSET..GENERAL_SYSCALL_OFFSET + 8],
            INTERNAL_SYSCALL_WORD,
        );
        assert_eq!(
            rel32_target(
                &patched,
                GENERAL_RETURN_JUMP_OFFSET + 1,
                GENERAL_RETURN_JUMP_OFFSET + 5,
            ),
            Some(COMMON_EPILOGUE_OFFSET),
        );
        assert_eq!(
            rel32_target(
                &patched,
                MARKER_FALLBACK_OFFSET + 11,
                MARKER_FALLBACK_OFFSET + 15,
            ),
            Some(COMMON_EPILOGUE_OFFSET),
        );
    }

    #[test]
    fn changed_or_truncated_function_bytes_are_refused() {
        for offset in [
            0,
            QUERY_BRANCH_OFFSET,
            QUERY_BODY_OFFSET,
            NORMAL_PATH_OFFSET,
            REDIRECT_OFFSET,
            GENERAL_SYSCALL_OFFSET,
            KNOWN_GETRANDOM.len() - 1,
        ] {
            let mut changed = KNOWN_GETRANDOM.to_vec();
            changed[offset] ^= 0x01;
            assert!(plan(Some(&changed)).is_err(), "changed byte {offset:#x}");
        }
        assert!(plan(Some(&KNOWN_GETRANDOM[..KNOWN_GETRANDOM.len() - 1])).is_err());
    }

    #[test]
    fn every_raw_fallback_syscall_word_mutation_is_refused() {
        let canonical = [SyntheticSymbol::canonical()];
        let original = synthetic_elf(&canonical, 0);
        assert_eq!(
            parsed_plan(&original).unwrap(),
            Some((
                SYNTHETIC_ELF_FUNCTION_OFFSET,
                plan(Some(KNOWN_GETRANDOM)).unwrap().unwrap(),
            )),
        );
        for word_offset in 0..INTERNAL_SYSCALL_WORD.len() {
            let mut changed = original.clone();
            changed[SYNTHETIC_ELF_FUNCTION_OFFSET + GENERAL_SYSCALL_OFFSET + word_offset] ^= 0x01;
            let error = parsed_plan(&changed).unwrap_err();
            let expected_reason = if word_offset + 1 == INTERNAL_SYSCALL_WORD.len() {
                "raw fallback syscall word changed"
            } else {
                "function bytes or size changed"
            };
            assert!(
                error.to_string().contains(expected_reason),
                "changed raw-fallback byte {word_offset} reached the wrong refusal: {error}"
            );
        }
    }

    #[test]
    fn stopped_target_routes_normal_getrandom_to_existing_raw_fallback() {
        let image = known_vdso_image();
        let mapping_start = 0x7fff_0000_0000;
        let plan = super::super::plan_stopped_getrandom(&image, mapping_start)
            .unwrap()
            .unwrap();
        let published = plan.expected_published_image().unwrap();
        let word = plan.publication_word().unwrap();
        assert_eq!(
            word.address,
            (mapping_start + FUNCTION_OFFSET as u64 + REDIRECT_OFFSET as u64) & !7,
        );

        let function = FUNCTION_OFFSET;
        assert_eq!(
            &published[function + QUERY_BRANCH_OFFSET..function + REDIRECT_OFFSET],
            &image[function + QUERY_BRANCH_OFFSET..function + REDIRECT_OFFSET],
        );
        assert_eq!(
            &published[function + QUERY_BODY_OFFSET..function + QUERY_RETURN_JUMP_OFFSET + 2],
            &image[function + QUERY_BODY_OFFSET..function + QUERY_RETURN_JUMP_OFFSET + 2],
        );
        assert_eq!(
            &published
                [function + GENERAL_FALLBACK_OFFSET..function + GENERAL_RETURN_JUMP_OFFSET + 5],
            &image[function + GENERAL_FALLBACK_OFFSET..function + GENERAL_RETURN_JUMP_OFFSET + 5],
        );
        assert_eq!(
            &published[function + GENERAL_SYSCALL_OFFSET
                ..function + GENERAL_SYSCALL_OFFSET + INTERNAL_SYSCALL_WORD.len()],
            INTERNAL_SYSCALL_WORD,
        );
        assert_eq!(
            &published[SGX_TARGET_LOAD_OFFSET..SGX_INDIRECT_CALL_OFFSET + 2],
            &image[SGX_TARGET_LOAD_OFFSET..SGX_INDIRECT_CALL_OFFSET + 2],
        );

        let subscriptions: reverie::Subscription = [
            reverie::syscalls::Sysno::getrandom,
            reverie::syscalls::Sysno::time,
            reverie::syscalls::Sysno::clock_gettime,
            reverie::syscalls::Sysno::getcpu,
            reverie::syscalls::Sysno::gettimeofday,
            reverie::syscalls::Sysno::clock_getres,
        ]
        .into_iter()
        .collect();
        let combined =
            super::super::plan_stopped_vdso(&image, mapping_start, &subscriptions).unwrap();
        assert!(combined.has_getrandom());
        assert_eq!(combined.legacy_patch_count(), 5);
        let expected = combined.expected_published_image().unwrap();
        let words = combined.publication_words().unwrap();
        assert_eq!(words[0], word);

        let mut replayed = image.clone();
        for word in &words {
            let offset = usize::try_from(word.address - mapping_start).unwrap();
            assert_eq!(
                &replayed[offset..offset + 8],
                word.expected.to_le_bytes().as_slice(),
            );
            replayed[offset..offset + 8].copy_from_slice(&word.replacement.to_le_bytes());
        }
        assert_eq!(replayed.as_slice(), expected.as_ref());
        for word in words.iter().rev() {
            let offset = usize::try_from(word.address - mapping_start).unwrap();
            assert_eq!(
                &replayed[offset..offset + 8],
                word.replacement.to_le_bytes().as_slice(),
            );
            replayed[offset..offset + 8].copy_from_slice(&word.expected.to_le_bytes());
        }
        assert_eq!(replayed, image);
    }
}
