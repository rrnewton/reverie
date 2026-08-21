# Kernel complaint text, extracted from the raw serial logs

One excerpt per (run, mechanism). The logs themselves are 271 MB of
mostly-identical boot output and are not committed; these are the lines
that carry the result. Regenerate with `extract_evidence.py` after a run.

### clocksource watchdog
runs/fine/kvm/t00-d19/serial.log

[   32.166755] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 19391765125 wd_nsec: 19391764634
HB 31.92 1787341390
HB 32.92 1787341391
HB 33.92 1787341392
HB 34.92 1787341393
HB 35.92 1787341394

### clocksource watchdog
runs/fine/kvm/t01-d20/serial.log

[   26.620891] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20351667632 wd_nsec: 20351667086
HB 26.37 1787341449
HB 27.37 1787341450
HB 28.37 1787341451
HB 29.38 1787341452
HB 30.38 1787341453

### soft-lockup detector
runs/fine/kvm/t02-d21/serial.log

[   30.134204] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:96]
[   30.134209] CPU#0 Utilization every 4s during lockup:
[   30.134222] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.134223] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.134224] 	#3:  15% system,	  0% softirq,	  3% hardirq,	  7% idle
[   30.134224] 	#4:   0% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/kvm/t02-d21/serial.log

[   30.134259] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21379638016 wd_nsec: 21379637384
HB 29.93 1787341510
HB 30.93 1787341511
HB 31.93 1787341512
HB 32.94 1787341513
HB 33.94 1787341514

### soft-lockup detector
runs/fine/kvm/t03-d22/serial.log

[   27.990303] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:96]
[   27.990308] CPU#0 Utilization every 4s during lockup:
[   27.990309] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   27.990310] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   27.990311] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   27.990312] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/fine/kvm/t03-d22/serial.log

[   27.990339] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22219379651 wd_nsec: 22219379159
HB 27.77 1787341569
HB 28.77 1787341570
HB 29.77 1787341571
HB 30.78 1787341572
HB 31.78 1787341573

### soft-lockup detector
runs/fine/kvm/t04-d23/serial.log

[   30.821772] watchdog: BUG: soft lockup - CPU#0 stuck for 24s! [sh:96]
[   30.821778] CPU#0 Utilization every 4s during lockup:
[   30.821778] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.821780] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.821780] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.821781] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/fine/kvm/t04-d23/serial.log

[   30.821808] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23048804113 wd_nsec: 23048803551
HB 30.60 1787341632
HB 31.60 1787341633
HB 32.60 1787341634
HB 33.61 1787341635
HB 34.61 1787341636

### clocksource watchdog
runs/fine/kvm/t05-d19/serial.log

[   25.551491] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 19285641108 wd_nsec: 19285640499
HB 25.30 1787341689
HB 26.31 1787341690
HB 27.31 1787341691
HB 28.31 1787341692
HB 29.31 1787341693

### soft-lockup detector
runs/fine/kvm/t06-d20/serial.log

[   28.259323] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:96]
[   28.259328] CPU#0 Utilization every 4s during lockup:
[   28.259339] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.259340] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.259341] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.259341] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/fine/kvm/t06-d20/serial.log

[   28.259374] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20007283955 wd_nsec: 20007283338
HB 28.05 1787341748
HB 29.06 1787341749
HB 30.06 1787341750
HB 31.06 1787341751
HB 32.06 1787341752

### soft-lockup detector
runs/fine/kvm/t07-d21/serial.log

[   31.644197] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:96]
[   31.644202] CPU#0 Utilization every 4s during lockup:
[   31.644203] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.644204] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.644205] 	#3:  15% system,	  0% softirq,	  3% hardirq,	  7% idle
[   31.644206] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### clocksource watchdog
runs/fine/kvm/t07-d21/serial.log

[   31.644232] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21374174046 wd_nsec: 21374173496
HB 31.42 1787341812
HB 32.43 1787341813
HB 33.43 1787341814
HB 34.43 1787341815
HB 35.43 1787341816

### soft-lockup detector
runs/fine/kvm/t08-d22/serial.log

[   34.866389] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:96]
[   34.866394] CPU#0 Utilization every 4s during lockup:
[   34.866395] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.866396] 	#2:  15% system,	  0% softirq,	  3% hardirq,	  7% idle
[   34.866397] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.866397] 	#4:   0% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/kvm/t08-d22/serial.log

[   34.894383] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22120955649 wd_nsec: 22120955089
HB 34.64 1787341878
HB 35.65 1787341879
HB 36.65 1787341880
HB 37.65 1787341881
HB 38.65 1787341882

### soft-lockup detector
runs/fine/kvm/t09-d23/serial.log

[   33.999349] watchdog: BUG: soft lockup - CPU#0 stuck for 24s! [sh:96]
[   33.999355] CPU#0 Utilization every 4s during lockup:
[   33.999355] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.999357] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.999357] 	#3:  15% system,	  0% softirq,	  3% hardirq,	  8% idle
[   33.999358] 	#4:   0% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/kvm/t09-d23/serial.log

[   33.999386] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23244531255 wd_nsec: 23244530677
HB 33.80 1787341944
HB 34.80 1787341945
HB 35.80 1787341946
HB 36.80 1787341947
HB 37.81 1787341948

### clocksource watchdog
runs/fine/kvm/t10-d19/serial.log

[   24.675576] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 19425120771 wd_nsec: 19425120149
HB 24.44 1787342000
HB 25.45 1787342001
HB 26.45 1787342002
HB 27.45 1787342003
HB 28.45 1787342004

### soft-lockup detector
runs/fine/kvm/t11-d20/serial.log

[   30.809753] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:96]
[   30.809758] CPU#0 Utilization every 4s during lockup:
[   30.809759] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.809760] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.809761] 	#3:  15% system,	  0% softirq,	  3% hardirq,	  7% idle
[   30.809762] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### clocksource watchdog
runs/fine/kvm/t11-d20/serial.log

[   30.809789] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20054203106 wd_nsec: 20054202427
HB 30.61 1787342062
HB 31.61 1787342063
HB 32.61 1787342064
HB 33.61 1787342065
HB 34.61 1787342066

### soft-lockup detector
runs/fine/kvm/t12-d21/serial.log

[   26.996462] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:96]
[   26.996467] CPU#0 Utilization every 4s during lockup:
[   26.996468] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   26.996469] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   26.996470] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   26.996470] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/fine/kvm/t12-d21/serial.log

[   26.996498] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21236293854 wd_nsec: 21236293346
HB 26.79 1787342121
HB 27.79 1787342122
HB 28.79 1787342123
HB 29.79 1787342124
HB 30.80 1787342125

### soft-lockup detector
runs/fine/kvm/t13-d22/serial.log

[   33.676177] watchdog: BUG: soft lockup - CPU#0 stuck for 24s! [sh:96]
[   33.676183] CPU#0 Utilization every 4s during lockup:
[   33.676184] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.676185] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.676186] 	#3:  15% system,	  0% softirq,	  3% hardirq,	  7% idle
[   33.676186] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### clocksource watchdog
runs/fine/kvm/t13-d22/serial.log

[   33.676215] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22425741802 wd_nsec: 22425741024
HB 33.47 1787342186
HB 34.47 1787342187
HB 35.48 1787342188
HB 36.48 1787342189
HB 37.48 1787342190

### soft-lockup detector
runs/fine/kvm/t14-d23/serial.log

[   30.873968] watchdog: BUG: soft lockup - CPU#0 stuck for 24s! [sh:96]
[   30.873973] CPU#0 Utilization every 4s during lockup:
[   30.873974] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.873975] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.873976] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.873976] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  8% idle

### clocksource watchdog
runs/fine/kvm/t14-d23/serial.log

[   30.874006] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23119614255 wd_nsec: 23119613781
HB 30.67 1787342248
HB 31.67 1787342249
HB 32.68 1787342250
HB 33.68 1787342251
HB 34.68 1787342252

### clocksource watchdog
runs/fine/tcg/t00-d19/serial.log

[   33.938353] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 19270047380 wd_nsec: 19269959230
HB 33.92 1787341397
HB 34.94 1787341398
HB 35.97 1787341399
HB 37.00 1787341400
HB 38.02 1787341401

### soft-lockup detector
runs/fine/tcg/t01-d20/serial.log

[   28.027549] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:98]
[   28.029217] CPU#0 Utilization every 4s during lockup:
[   28.029291] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.029417] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.029442] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.029463] 	#4:  43% system,	  1% softirq,	  2% hardirq,	 14% idle

### clocksource watchdog
runs/fine/tcg/t01-d20/serial.log

[   28.034550] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20367454453 wd_nsec: 20367336650
HB 28.01 1787341460
HB 29.03 1787341461
HB 30.05 1787341462
HB 31.07 1787341463
HB 32.10 1787341464

### soft-lockup detector
runs/fine/tcg/t02-d21/serial.log

[   31.572875] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:97]
[   31.574694] CPU#0 Utilization every 4s during lockup:
[   31.574774] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.574927] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.574954] 	#3:  45% system,	  1% softirq,	  3% hardirq,	 14% idle
[   31.574977] 	#4:   2% system,	  1% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/tcg/t02-d21/serial.log

[   31.580293] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21408889927 wd_nsec: 21408765730
HB 31.55 1787341528
HB 32.57 1787341529
HB 33.60 1787341530
HB 34.63 1787341531
HB 35.65 1787341532

### soft-lockup detector
runs/fine/tcg/t03-d22/serial.log

[   29.473004] watchdog: BUG: soft lockup - CPU#0 stuck for 23s! [sh:97]
[   29.474802] CPU#0 Utilization every 4s during lockup:
[   29.474880] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.475015] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.475041] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.475068] 	#4:  44% system,	  1% softirq,	  3% hardirq,	 14% idle

### clocksource watchdog
runs/fine/tcg/t03-d22/serial.log

[   29.480444] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22309959824 wd_nsec: 22309831280
HB 29.45 1787341594
HB 30.47 1787341595
HB 31.50 1787341596
HB 32.52 1787341597
HB 33.55 1787341598

### soft-lockup detector
runs/fine/tcg/t04-d23/serial.log

[   32.189521] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:98]
[   32.191293] CPU#0 Utilization every 4s during lockup:
[   32.191372] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.191501] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.191527] 	#3:  43% system,	  1% softirq,	  2% hardirq,	 14% idle
[   32.191551] 	#4:   1% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/tcg/t04-d23/serial.log

[   32.196770] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23031738314 wd_nsec: 23031622210
HB 32.17 1787341661
HB 33.19 1787341662
HB 34.21 1787341663
HB 35.23 1787341664
HB 36.26 1787341665

### soft-lockup detector
runs/fine/tcg/t05-d19/serial.log

[   27.027148] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:98]
[   27.028963] CPU#0 Utilization every 4s during lockup:
[   27.029042] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   27.029176] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   27.029220] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   27.029243] 	#4:  45% system,	  1% softirq,	  3% hardirq,	 14% idle

### clocksource watchdog
runs/fine/tcg/t05-d19/serial.log

[   27.034713] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 19360025659 wd_nsec: 19359915720
HB 27.01 1787341725
HB 28.03 1787341726
HB 29.05 1787341727
HB 30.08 1787341728
HB 31.10 1787341729

### clocksource watchdog
runs/fine/tcg/t06-d20/serial.log

[   29.638719] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20472228623 wd_nsec: 20472111240
HB 29.62 1787341791
HB 30.64 1787341792
HB 31.66 1787341793
HB 32.69 1787341794
HB 33.71 1787341795

### soft-lockup detector
runs/fine/tcg/t07-d21/serial.log

[   33.161018] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:98]
[   33.162806] CPU#0 Utilization every 4s during lockup:
[   33.162885] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.163017] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.163044] 	#3:  45% system,	  1% softirq,	  3% hardirq,	 14% idle
[   33.163067] 	#4:   2% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/tcg/t07-d21/serial.log

[   33.168429] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21494021366 wd_nsec: 21493899860
HB 33.14 1787341859
HB 34.16 1787341860
HB 35.19 1787341861
HB 36.22 1787341862
HB 37.25 1787341863

### soft-lockup detector
runs/fine/tcg/t08-d22/serial.log

[   36.347749] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:98]
[   36.349600] CPU#0 Utilization every 4s during lockup:
[   36.349679] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.349812] 	#2:  44% system,	  1% softirq,	  3% hardirq,	 14% idle
[   36.349839] 	#3:   1% system,	  0% softirq,	  1% hardirq,	  0% idle
[   36.349862] 	#4:   2% system,	  1% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/tcg/t08-d22/serial.log

[   36.355151] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22183872648 wd_nsec: 22183745270
HB 36.33 1787341932
HB 37.35 1787341933
HB 38.37 1787341934
HB 39.39 1787341935
HB 40.42 1787341937

### soft-lockup detector
runs/fine/tcg/t09-d23/serial.log

[   35.389538] watchdog: BUG: soft lockup - CPU#0 stuck for 24s! [sh:98]
[   35.391213] CPU#0 Utilization every 4s during lockup:
[   35.391287] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.391416] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.391440] 	#3:  43% system,	  1% softirq,	  2% hardirq,	 14% idle
[   35.391463] 	#4:   1% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/tcg/t09-d23/serial.log

[   35.396562] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23233427884 wd_nsec: 23233295210
HB 35.37 1787342004
HB 36.39 1787342005
HB 37.42 1787342006
HB 38.45 1787342007
HB 39.47 1787342008

### clocksource watchdog
runs/fine/tcg/t10-d19/serial.log

[   26.145758] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 19469036150 wd_nsec: 19468925150
HB 26.12 1787342065
HB 27.15 1787342066
HB 28.18 1787342067
HB 29.20 1787342068
HB 30.23 1787342069

### soft-lockup detector
runs/fine/tcg/t11-d20/serial.log

[   32.219355] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:98]
[   32.221187] CPU#0 Utilization every 4s during lockup:
[   32.221266] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.221402] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.221429] 	#3:  44% system,	  1% softirq,	  3% hardirq,	 14% idle
[   32.221453] 	#4:   2% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/fine/tcg/t11-d20/serial.log

[   32.226772] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20055400963 wd_nsec: 20055286120
HB 32.20 1787342133
HB 33.22 1787342134
HB 34.25 1787342135
HB 35.27 1787342136
HB 36.30 1787342137

### soft-lockup detector
runs/fine/tcg/t12-d21/serial.log

[   28.469129] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:98]
[   28.471738] CPU#0 Utilization every 4s during lockup:
[   28.471866] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.472056] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.472095] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   28.472130] 	#4:  43% system,	  1% softirq,	  2% hardirq,	 14% idle

### clocksource watchdog
runs/fine/tcg/t12-d21/serial.log

[   28.480272] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21315847621 wd_nsec: 21315725930
HB 28.46 1787342199
HB 29.48 1787342200
HB 30.51 1787342201
HB 31.53 1787342202
HB 32.56 1787342203

### RCU stall detector
runs/fine/tcg/t13-d22/serial.log

[   35.105471] rcu: INFO: rcu_sched self-detected stall on CPU
[   35.105887] rcu: 	0-...!: (1 ticks this GP) idle=02a4/1/0x4000000000000000 softirq=1385/1385 fqs=0
[   35.106087] rcu: 	(t=22001 jiffies g=-59 q=14 ncpus=1)
[   35.106193] rcu: rcu_sched kthread timer wakeup didn't happen for 21998 jiffies! g-59 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[   35.106327] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=368
[   35.106455] rcu: rcu_sched kthread starved for 22001 jiffies! g-59 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### clocksource watchdog
runs/fine/tcg/t13-d22/serial.log

[   35.113347] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22437875282 wd_nsec: 22437741610
HB 57.26 1787342291
HB 58.29 1787342292
HB 59.31 1787342293
HB 60.33 1787342294
HB 61.36 1787342295

### soft-lockup detector
runs/fine/tcg/t14-d23/serial.log

[   32.205541] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:98]
[   32.207364] CPU#0 Utilization every 4s during lockup:
[   32.207443] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.207578] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.207605] 	#3:  43% system,	  1% softirq,	  2% hardirq,	 14% idle
[   32.207629] 	#4:   2% system,	  0% softirq,	  1% hardirq,	  0% idle

### hrtimer overrun report
runs/fine/tcg/t14-d23/serial.log

[   32.213017] hrtimer: interrupt took 8502529 ns
[   32.213208] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23048184830 wd_nsec: 23048059760
HB 32.19 1787342337
HB 33.21 1787342338
HB 34.23 1787342339
HB 35.26 1787342340

### clocksource watchdog
runs/fine/tcg/t14-d23/serial.log

[   32.213208] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 23048184830 wd_nsec: 23048059760
HB 32.19 1787342337
HB 33.21 1787342338
HB 34.23 1787342339
HB 35.26 1787342340
HB 36.28 1787342341

### clocksource watchdog
runs/main-qmp/kvm/d10/serial.log

[    6.269555] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 10495297735 wd_nsec: 495999728
HB 6.12 1787340632
HB 7.12 1787340633
HB 8.12 1787340634
HB 9.12 1787340635
HB 10.13 1787340636

### clocksource watchdog
runs/main-qmp/kvm/d125/serial.log

[    8.250383] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 125495967443 wd_nsec: 496000410
HB 8.07 1787340921
HB 9.08 1787340922
HB 10.08 1787340923
HB 11.08 1787340924
HB 12.08 1787340925

### clocksource watchdog
runs/main-qmp/kvm/d21/serial.log

[   13.936886] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 21504082015 wd_nsec: 504000155
HB 14.45 1787340691
HB 15.45 1787340692
HB 16.46 1787340693
HB 17.46 1787340694
HB 18.46 1787340695

### clocksource watchdog
runs/main-qmp/kvm/d3/serial.log

[   13.937701] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 3504163452 wd_nsec: 503999984
HB 14.62 1787340591
HB 15.63 1787340592
HB 16.63 1787340593
HB 17.63 1787340594
HB 18.63 1787340595

### clocksource watchdog
runs/main-qmp/kvm/d30/serial.log

[    7.894220] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 30503991297 wd_nsec: 503999946
HB 8.20 1787340751
HB 9.20 1787340752
HB 10.21 1787340753
HB 11.21 1787340754
HB 12.21 1787340755

### clocksource watchdog
runs/main-qmp/kvm/d300/serial.log

[    7.754805] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 300504101909 wd_nsec: 504001307
HB 8.06 1787341085
HB 9.07 1787341086
HB 10.07 1787341087
HB 11.07 1787341088
HB 12.07 1787341089

### clocksource watchdog
runs/main-qmp/kvm/d60/serial.log

[    9.633312] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 60496116943 wd_nsec: 496005168
HB 9.45 1787340822
HB 10.45 1787340823
HB 11.45 1787340824
HB 12.46 1787340825
HB 13.46 1787340826

### clocksource watchdog
runs/main-sig/kvm/d10/serial.log

[   16.983927] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 10234502388 wd_nsec: 10234502425
HB 16.75 1787340644
HB 17.75 1787340645
HB 18.75 1787340646
HB 19.76 1787340647
HB 20.76 1787340648

### soft-lockup detector
runs/main-sig/kvm/d125/serial.log

[  131.829529] watchdog: BUG: soft lockup - CPU#0 stuck for 118s! [sh:96]
[  131.829534] CPU#0 Utilization every 4s during lockup:
[  131.829535] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  131.829536] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  131.829537] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  131.829537] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/main-sig/kvm/d125/serial.log

[  131.829565] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 125067843390 wd_nsec: 125067840301
HB 131.62 1787341369
HB 132.62 1787341370
HB 133.62 1787341371
HB 134.63 1787341372
HB 135.63 1787341373

### clocksource watchdog
runs/main-sig/kvm/d15/serial.log

[   28.447510] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 15097025780 wd_nsec: 15097025255
HB 28.21 1787340706
HB 29.21 1787340707
HB 30.22 1787340708
HB 31.22 1787340709
HB 32.22 1787340710

### clocksource watchdog
runs/main-sig/kvm/d18/serial.log

[   25.626514] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 18377367209 wd_nsec: 18377366852
HB 25.40 1787340764
HB 26.40 1787340765
HB 27.40 1787340766
HB 28.40 1787340767
HB 29.40 1787340768

### clocksource watchdog
runs/main-sig/kvm/d20/serial.log

[   29.180923] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20425729084 wd_nsec: 20425728516
HB 28.95 1787340824
HB 29.95 1787340825
HB 30.95 1787340826
HB 31.96 1787340827
HB 32.96 1787340828

### soft-lockup detector
runs/main-sig/kvm/d22/serial.log

[   29.841277] watchdog: BUG: soft lockup - CPU#0 stuck for 23s! [sh:96]
[   29.841282] CPU#0 Utilization every 4s during lockup:
[   29.841283] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.841285] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.841285] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.841286] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  8% idle

### clocksource watchdog
runs/main-sig/kvm/d22/serial.log

[   29.841313] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22084463686 wd_nsec: 22084463128
HB 29.64 1787340885
HB 30.64 1787340886
HB 31.64 1787340887
HB 32.64 1787340888
HB 33.64 1787340889

### soft-lockup detector
runs/main-sig/kvm/d25/serial.log

[   32.505871] watchdog: BUG: soft lockup - CPU#0 stuck for 26s! [sh:96]
[   32.505876] CPU#0 Utilization every 4s during lockup:
[   32.505877] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.505878] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.505879] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.505879] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/main-sig/kvm/d25/serial.log

[   32.505910] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 25237468655 wd_nsec: 25237468049
HB 32.29 1787340950
HB 33.29 1787340951
HB 34.29 1787340952
HB 35.29 1787340953
HB 36.29 1787340954

### soft-lockup detector
runs/main-sig/kvm/d30/serial.log

[   36.905466] watchdog: BUG: soft lockup - CPU#0 stuck for 30s! [sh:96]
[   36.905471] CPU#0 Utilization every 4s during lockup:
[   36.905472] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.905473] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.905474] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.905474] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  7% idle

### clocksource watchdog
runs/main-sig/kvm/d30/serial.log

[   36.905502] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 30151471972 wd_nsec: 30151471222
HB 36.70 1787341018
HB 37.71 1787341019
HB 38.71 1787341020
HB 39.71 1787341021
HB 40.71 1787341022

### soft-lockup detector
runs/main-sig/kvm/d45/serial.log

[   52.549823] watchdog: BUG: soft lockup - CPU#0 stuck for 44s! [sh:96]
[   52.549829] CPU#0 Utilization every 4s during lockup:
[   52.549830] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   52.549831] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   52.549832] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   52.549832] 	#4:  15% system,	  0% softirq,	  3% hardirq,	  8% idle

### clocksource watchdog
runs/main-sig/kvm/d45/serial.log

[   52.549861] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 45298869233 wd_nsec: 45298868106
HB 52.35 1787341103
HB 53.35 1787341104
HB 54.35 1787341105
HB 55.35 1787341106
HB 56.35 1787341107

### clocksource watchdog
runs/main-sig/kvm/d5/serial.log

[   18.767102] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 5247930440 wd_nsec: 5247930189
HB 18.52 1787340595
HB 19.53 1787340596
HB 20.53 1787340597
HB 21.53 1787340598
HB 22.53 1787340599

### soft-lockup detector
runs/main-sig/kvm/d60/serial.log

[   71.177630] watchdog: BUG: soft lockup - CPU#0 stuck for 59s! [sh:96]
[   71.177635] CPU#0 Utilization every 4s during lockup:
[   71.177636] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   71.177638] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   71.177638] 	#3:  15% system,	  0% softirq,	  3% hardirq,	  8% idle
[   71.177639] 	#4:   0% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/main-sig/kvm/d60/serial.log

[   71.205144] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 60434524483 wd_nsec: 60434523112
HB 70.96 1787341205
HB 71.96 1787341206
HB 72.96 1787341207
HB 73.96 1787341208
HB 74.96 1787341209

### clocksource watchdog
runs/main-sig/tcg/d10/serial.log

[   17.593892] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 10425968434 wd_nsec: 10425915260
HB 17.57 1787340658
HB 18.60 1787340659
HB 19.62 1787340660
HB 20.65 1787340661
HB 21.68 1787340662

### soft-lockup detector
runs/main-sig/tcg/d125/serial.log

[  133.305195] watchdog: BUG: soft lockup - CPU#0 stuck for 120s! [sh:98]
[  133.307027] CPU#0 Utilization every 4s during lockup:
[  133.307107] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  133.307243] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  133.307270] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  133.307293] 	#4:  45% system,	  1% softirq,	  3% hardirq,	 14% idle

### clocksource watchdog
runs/main-sig/tcg/d125/serial.log

[  133.312680] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 125142266604 wd_nsec: 0
HB 133.29 1787341433
HB 134.31 1787341434
HB 135.33 1787341435
HB 136.36 1787341436
HB 137.38 1787341437

### clocksource watchdog
runs/main-sig/tcg/d15/serial.log

[   29.851208] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 15180166606 wd_nsec: 15180081090
HB 29.83 1787340723
HB 30.86 1787340724
HB 31.88 1787340725
HB 32.91 1787340726
HB 33.94 1787340727

### clocksource watchdog
runs/main-sig/tcg/d18/serial.log

[   27.253038] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 18081251250 wd_nsec: 18081149970
HB 27.23 1787340786
HB 28.26 1787340787
HB 29.29 1787340788
HB 30.31 1787340789
HB 31.34 1787340790

### clocksource watchdog
runs/main-sig/tcg/d20/serial.log

[   30.669423] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 20503083494 wd_nsec: 20502969500
HB 30.65 1787340854
HB 31.68 1787340855
HB 32.71 1787340856
HB 33.73 1787340857
HB 34.76 1787340858

### soft-lockup detector
runs/main-sig/tcg/d22/serial.log

[   31.352051] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:98]
[   31.353773] CPU#0 Utilization every 4s during lockup:
[   31.353848] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.353975] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.353999] 	#3:  45% system,	  1% softirq,	  3% hardirq,	 14% idle
[   31.354022] 	#4:   1% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/main-sig/tcg/d22/serial.log

[   31.359155] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22185352932 wd_nsec: 22185215170
HB 31.33 1787340919
HB 32.35 1787340920
HB 33.37 1787340921
HB 34.40 1787340922
HB 35.43 1787340923

### soft-lockup detector
runs/main-sig/tcg/d25/serial.log

[   34.107098] watchdog: BUG: soft lockup - CPU#0 stuck for 23s! [sh:98]
[   34.108852] CPU#0 Utilization every 4s during lockup:
[   34.108930] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.109063] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.109089] 	#3:  46% system,	  1% softirq,	  3% hardirq,	 14% idle
[   34.109113] 	#4:   2% system,	  1% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/main-sig/tcg/d25/serial.log

[   34.114399] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 25436778621 wd_nsec: 25436636380
HB 34.09 1787340991
HB 35.11 1787340992
HB 36.14 1787340993
HB 37.16 1787340994
HB 38.19 1787340995

### soft-lockup detector
runs/main-sig/tcg/d30/serial.log

[   39.225222] watchdog: BUG: soft lockup - CPU#0 stuck for 28s! [sh:98]
[   39.227101] CPU#0 Utilization every 4s during lockup:
[   39.227181] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   39.227316] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   39.227342] 	#3:  45% system,	  1% softirq,	 26% hardirq,	 11% idle
[   39.227366] 	#4:   1% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/main-sig/tcg/d30/serial.log

[   39.232940] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 30065347113 wd_nsec: 30065175360
HB 39.21 1787341065
HB 40.23 1787341066
HB 41.26 1787341067
HB 42.30 1787341068
HB 43.32 1787341069

### soft-lockup detector
runs/main-sig/tcg/d45/serial.log

[   54.038926] watchdog: BUG: soft lockup - CPU#0 stuck for 42s! [sh:98]
[   54.040731] CPU#0 Utilization every 4s during lockup:
[   54.040810] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.040945] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.040971] 	#3:  45% system,	  1% softirq,	  3% hardirq,	 14% idle
[   54.040995] 	#4:   2% system,	  1% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/main-sig/tcg/d45/serial.log

[   54.046341] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 45369803138 wd_nsec: 2419865810
HB 54.02 1787341155
HB 55.04 1787341156
HB 56.07 1787341157
HB 57.09 1787341158
HB 58.12 1787341159

### clocksource watchdog
runs/main-sig/tcg/d5/serial.log

[   21.109315] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 5010914534 wd_nsec: 5010889050
HB 21.03 1787340603
HB 22.05 1787340604
HB 23.08 1787340605
HB 24.10 1787340606
HB 25.13 1787340607

### soft-lockup detector
runs/main-sig/tcg/d60/serial.log

[   72.555648] watchdog: BUG: soft lockup - CPU#0 stuck for 59s! [sh:97]
[   72.557490] CPU#0 Utilization every 4s during lockup:
[   72.557568] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   72.557702] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   72.557729] 	#3:  43% system,	  1% softirq,	  2% hardirq,	 14% idle
[   72.557752] 	#4:   2% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/main-sig/tcg/d60/serial.log

[   72.563401] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 60399490020 wd_nsec: 17449455180
HB 72.54 1787341264
HB 73.56 1787341265
HB 74.59 1787341266
HB 75.62 1787341267
HB 76.64 1787341268

### clocksource watchdog
runs/probe/kvm/d3/serial.log

[   13.753775] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 3504163592 wd_nsec: 503999995
HB 14.08 1787340367
HB 15.08 1787340368
HB 16.08 1787340369
HB 17.08 1787340370
HB 18.08 1787340371

### soft-lockup detector
runs/probe/kvm/d30/serial.log

[   43.241292] watchdog: BUG: soft lockup - CPU#0 stuck for 29s! [sh:96]
[   43.241297] CPU#0 Utilization every 4s during lockup:
[   43.241298] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   43.241299] 	#2:  15% system,	  0% softirq,	  3% hardirq,	  7% idle
[   43.241300] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   43.241300] 	#4:   0% system,	  0% softirq,	  1% hardirq,	  0% idle

### clocksource watchdog
runs/probe/kvm/d30/serial.log

[   43.241329] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 30464909999 wd_nsec: 30464909092
HB 43.02 1787340532
HB 44.02 1787340533
HB 45.02 1787340534
HB 46.03 1787340535
HB 47.03 1787340536

### RCU stall detector
runs/rcu/kvm/t00-d22/serial.log

[   29.173883] rcu: INFO: rcu_sched self-detected stall on CPU
[   29.175154] rcu: 	0-...!: (1 ticks this GP) idle=0204/1/0x4000000000000000 softirq=92280/92280 fqs=0
[   29.177106] rcu: 	(t=22000 jiffies g=41725 q=370 ncpus=1)
[   29.178263] rcu: rcu_sched kthread timer wakeup didn't happen for 21997 jiffies! g41725 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[   29.180609] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=2021
[   29.182066] rcu: rcu_sched kthread starved for 22000 jiffies! g41725 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### clocksource watchdog
runs/rcu/kvm/t00-d22/serial.log

[   29.238883] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 22343898983 wd_nsec: 22343898558
HB 40.75 1787341428
HB 49.24 1787341434
ls: /proc/4769: No such file or directory
ls: /proc/5002: No such file or directory
ls: /proc/5346: No such file or directory

### clocksource watchdog
runs/rcu/kvm/t00-d30/serial.log

[    7.335538] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 30495970400 wd_nsec: 495996043
ls: /proc/6344: No such file or directory
ls: /proc/6469: No such file or directory
ls: /proc/6510: No such file or directory
ls: /proc/6555: No such file or directory
ls: /proc/6729: No such file or directory

### clocksource watchdog
runs/rcu/kvm/t01-d125/serial.log

[    8.773143] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 125504065445 wd_nsec: 503998582
ls: /proc/15678: No such file or directory
ls: /proc/15814: No such file or directory
ls: /proc/15923: No such file or directory
ls: /proc/16050: No such file or directory
ls: /proc/16114: No such file or directory

### RCU stall detector
runs/rcu/kvm/t01-d25/serial.log

[   33.563492] rcu: INFO: rcu_sched self-detected stall on CPU
[   33.564797] rcu: 	0-...!: (1 ticks this GP) idle=0214/1/0x4000000000000000 softirq=121007/121007 fqs=0
[   33.566861] rcu: 	(t=25000 jiffies g=54841 q=236 ncpus=1)
[   33.568077] rcu: rcu_sched kthread timer wakeup didn't happen for 24997 jiffies! g54841 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[   33.570545] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=2554
[   33.572102] rcu: rcu_sched kthread starved for 25000 jiffies! g54841 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### clocksource watchdog
runs/rcu/kvm/t01-d25/serial.log

[   33.644787] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 25377016591 wd_nsec: 25377016051
HB 47.29 1787341506
HB 56.17 1787341512
ls: /proc/16259: No such file or directory
ls: /proc/16346: No such file or directory
HB 58.65 1787341514

### RCU stall detector
runs/rcu/kvm/t02-d30/serial.log

[   41.885668] rcu: INFO: rcu_sched self-detected stall on CPU
[   41.886936] rcu: 	0-...!: (1 ticks this GP) idle=0314/1/0x4000000000000000 softirq=175653/175653 fqs=0
[   41.888888] rcu: 	(t=30000 jiffies g=79637 q=289 ncpus=1)
[   41.890030] rcu: rcu_sched kthread timer wakeup didn't happen for 29997 jiffies! g79637 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[   41.892398] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=3718
[   41.893844] rcu: rcu_sched kthread starved for 30000 jiffies! g79637 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### clocksource watchdog
runs/rcu/kvm/t02-d30/serial.log

[   41.931809] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 30172684395 wd_nsec: 30172683655
HB 59.35 1787341594
HB 69.98 1787341601
ls: /proc/6010: No such file or directory
ls: /proc/6080: No such file or directory
ls: /proc/6176: No such file or directory

### clocksource watchdog
runs/rcu/kvm/t02-d300/serial.log

[   12.249375] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 300495993942 wd_nsec: 495999378
ls: /proc/8215: No such file or directory
ls: /proc/8236: No such file or directory
ls: /proc/8252: No such file or directory
ls: /proc/8269: No such file or directory
ls: /proc/8347: No such file or directory

### RCU stall detector
runs/rcu/kvm/t03-d45/serial.log

[   52.272985] rcu: INFO: rcu_sched self-detected stall on CPU
[   52.274275] rcu: 	0-...!: (1 ticks this GP) idle=0304/1/0x4000000000000000 softirq=98219/98219 fqs=0
[   52.276176] rcu: 	(t=45000 jiffies g=44921 q=59 ncpus=1)
[   52.277296] rcu: rcu_sched kthread timer wakeup didn't happen for 44997 jiffies! g44921 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[   52.279628] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=2117
[   52.281101] rcu: rcu_sched kthread starved for 45000 jiffies! g44921 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### clocksource watchdog
runs/rcu/kvm/t03-d45/serial.log

[   52.353688] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 45099064693 wd_nsec: 45099063561
ls: /proc/7033: No such file or directory
HB 76.42 1787341696

### RCU stall detector
runs/rcu/kvm/t04-d60/serial.log

[   71.478661] rcu: INFO: rcu_sched self-detected stall on CPU
[   71.480019] rcu: 	0-...!: (1 ticks this GP) idle=01f4/1/0x4000000000000000 softirq=164510/164510 fqs=0
[   71.482095] rcu: 	(t=60001 jiffies g=75289 q=719 ncpus=1)
[   71.483265] rcu: rcu_sched kthread timer wakeup didn't happen for 59998 jiffies! g75289 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[   71.485684] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=3571
[   71.487223] rcu: rcu_sched kthread starved for 60001 jiffies! g75289 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### clocksource watchdog
runs/rcu/kvm/t04-d60/serial.log

[   71.550007] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 60280586854 wd_nsec: 60280585265

### RCU stall detector
runs/rcu/kvm/t05-d125/serial.log

[  132.720452] rcu: INFO: rcu_sched self-detected stall on CPU
[  132.721758] rcu: 	0-...!: (1 ticks this GP) idle=02e4/1/0x4000000000000000 softirq=106880/106880 fqs=0
[  132.723721] rcu: 	(t=125000 jiffies g=48517 q=71 ncpus=1)
[  132.724891] rcu: rcu_sched kthread timer wakeup didn't happen for 124997 jiffies! g48517 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402
[  132.727314] rcu: 	Possible timer handling issue on cpu=0 timer-softirq=2276
[  132.728791] rcu: rcu_sched kthread starved for 125000 jiffies! g48517 f0x0 RCU_GP_WAIT_FQS(5) ->state=0x402 ->cpu=0

### workqueue watchdog
runs/rcu/kvm/t05-d125/serial.log

[  165.073557] BUG: workqueue lockup - pool cpus=0 node=0 flags=0x0 nice=0 stuck for 32s!
[  165.075335] Showing busy workqueues and worker pools:
[  165.076438] workqueue events: flags=0x0
[  165.077291]   pwq 2: cpus=0 node=0 flags=0x0 nice=0 active=4 refcnt=5
[  165.077296]     pending: psi_avgs_work, deferred_probe_timeout_work_func, regulator_init_complete_work_function, vmstat_shepherd
[  165.077315] workqueue events_unbound: flags=0x2

### clocksource watchdog
runs/rcu/kvm/t05-d125/serial.log

[  132.791177] clocksource: Long readout interval, skipping watchdog check: cs_nsec: 125541633633 wd_nsec: 125541630392
[  165.073557] BUG: workqueue lockup - pool cpus=0 node=0 flags=0x0 nice=0 stuck for 32s!
[  165.075335] Showing busy workqueues and worker pools:
[  165.076438] workqueue events: flags=0x0
[  165.077291]   pwq 2: cpus=0 node=0 flags=0x0 nice=0 active=4 refcnt=5
[  165.077296]     pending: psi_avgs_work, deferred_probe_timeout_work_func, regulator_init_complete_work_function, vmstat_shepherd

### soft-lockup detector
runs/smoke5/kvm/t00-d30/serial.log

[   45.445524] watchdog: BUG: soft lockup - CPU#0 stuck for 31s! [sh:133]
[   45.445533] CPU#0 Utilization every 4s during lockup:
[   45.445534] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   45.445535] 	#2:   3% system,	  0% softirq,	  0% hardirq,	 25% idle
[   45.445536] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   45.445537] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smoke6/kvm/t00-d25/serial.log

[   40.437720] watchdog: BUG: soft lockup - CPU#0 stuck for 26s! [sh:130]
[   40.437726] CPU#0 Utilization every 4s during lockup:
[   40.437726] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   40.437728] 	#2:   3% system,	  0% softirq,	  0% hardirq,	 24% idle
[   40.437728] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   40.437729] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t02-d21/serial.log

[   32.293398] watchdog: BUG: soft lockup - CPU#1 stuck for 23s! [sh:130]
[   32.293403] CPU#1 Utilization every 4s during lockup:
[   32.293404] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.293406] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.293406] 	#3:   2% system,	  0% softirq,	  0% hardirq,	 25% idle
[   32.293407] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t03-d22/serial.log

[   30.143955] watchdog: BUG: soft lockup - CPU#1 stuck for 24s! [sh:135]
[   30.143960] CPU#1 Utilization every 4s during lockup:
[   30.143961] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.143962] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.143963] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.143964] 	#4:   3% system,	  0% softirq,	  0% hardirq,	 20% idle

### soft-lockup detector
runs/smp-fine/kvm/t04-d23/serial.log

[   32.967513] watchdog: BUG: soft lockup - CPU#2 stuck for 23s! [sh:131]
[   32.967519] CPU#2 Utilization every 4s during lockup:
[   32.967526] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.967527] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.967528] 	#3:   7% system,	  0% softirq,	  0% hardirq,	 16% idle
[   32.967529] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t06-d20/serial.log

[   30.309499] watchdog: BUG: soft lockup - CPU#2 stuck for 21s! [sh:132]
[   30.309504] CPU#2 Utilization every 4s during lockup:
[   30.309517] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.309518] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.309519] 	#3:   1% system,	  0% softirq,	  0% hardirq,	 23% idle
[   30.309519] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t08-d22/serial.log

[   36.931036] watchdog: BUG: soft lockup - CPU#1 stuck for 23s! [sh:134]
[   36.931042] CPU#1 Utilization every 4s during lockup:
[   36.931042] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.931044] 	#2:   2% system,	  0% softirq,	  0% hardirq,	 22% idle
[   36.931045] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.931045] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t09-d23/serial.log

[   36.108787] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:129]
[   36.108792] CPU#0 Utilization every 4s during lockup:
[   36.108793] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.108794] 	#2:   2% system,	  0% softirq,	  0% hardirq,	 26% idle
[   36.108795] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.108796] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t12-d21/serial.log

[   29.194900] watchdog: BUG: soft lockup - CPU#2 stuck for 23s! [sh:131]
[   29.194905] CPU#2 Utilization every 4s during lockup:
[   29.194906] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.194907] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.194908] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   29.194909] 	#4:   8% system,	  0% softirq,	  0% hardirq,	 14% idle

### soft-lockup detector
runs/smp-fine/kvm/t13-d22/serial.log

[   35.808232] watchdog: BUG: soft lockup - CPU#1 stuck for 22s! [sh:132]
[   35.808237] CPU#1 Utilization every 4s during lockup:
[   35.808238] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.808239] 	#2:   1% system,	  0% softirq,	  0% hardirq,	 25% idle
[   35.808240] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.808241] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/kvm/t14-d23/serial.log

[   32.967214] watchdog: BUG: soft lockup - CPU#0 stuck for 23s! [sh:132]
[   32.967219] CPU#0 Utilization every 4s during lockup:
[   32.967220] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.967222] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.967222] 	#3:   5% system,	  0% softirq,	  0% hardirq,	 21% idle
[   32.967223] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t00-d19/serial.log

[   35.482747] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:132]
[   35.482886] CPU#0 Utilization every 4s during lockup:
[   35.482886] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.482886] 	#2:  23% system,	  0% softirq,	  0% hardirq,	 29% idle
[   35.482886] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.482886] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t02-d21/serial.log

[   33.422469] watchdog: BUG: soft lockup - CPU#1 stuck for 23s! [sh:131]
[   33.422469] CPU#1 Utilization every 4s during lockup:
[   33.422469] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.422469] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.422469] 	#3:  14% system,	  0% softirq,	  0% hardirq,	 25% idle
[   33.422469] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t03-d22/serial.log

[   31.221348] watchdog: BUG: soft lockup - CPU#2 stuck for 21s! [sh:134]
[   31.221708] CPU#2 Utilization every 4s during lockup:
[   31.221708] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.221708] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.221708] 	#3:  14% system,	  0% softirq,	  0% hardirq,	 30% idle
[   31.221708] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t04-d23/serial.log

[   34.226374] watchdog: BUG: soft lockup - CPU#3 stuck for 23s! [sh:133]
[   34.226647] CPU#3 Utilization every 4s during lockup:
[   34.226647] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.226647] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.226647] 	#3:  22% system,	  0% softirq,	  0% hardirq,	 15% idle
[   34.226647] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t06-d20/serial.log

[   31.735901] watchdog: BUG: soft lockup - CPU#2 stuck for 21s! [sh:129]
[   31.735934] CPU#2 Utilization every 4s during lockup:
[   31.735934] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.735934] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.735934] 	#3:   3% system,	  0% softirq,	  0% hardirq,	 47% idle
[   31.735934] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t07-d21/serial.log

[   34.993998] watchdog: BUG: soft lockup - CPU#2 stuck for 21s! [sh:135]
[   34.993998] CPU#2 Utilization every 4s during lockup:
[   34.993998] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.993998] 	#2:  10% system,	  0% softirq,	  0% hardirq,	 37% idle
[   34.993998] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.993998] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t08-d22/serial.log

[   38.201222] watchdog: BUG: soft lockup - CPU#3 stuck for 24s! [sh:132]
[   38.201491] CPU#3 Utilization every 4s during lockup:
[   38.201491] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   38.201491] 	#2:  11% system,	  0% softirq,	  0% hardirq,	 35% idle
[   38.201491] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   38.201491] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### hrtimer overrun report
runs/smp-fine/tcg/t08-d22/serial.log

[   38.213302] hrtimer: interrupt took 13132830 ns
HB 38.18 1787347763 1.77 5/123
HB 39.23 1787347764 1.77 5/123
HB 40.26 1787347765 1.95 6/123
HB 41.29 1787347766 1.95 5/123
HB 42.33 1787347767 1.95 5/123

### soft-lockup detector
runs/smp-fine/tcg/t09-d23/serial.log

[   37.303301] watchdog: BUG: soft lockup - CPU#2 stuck for 23s! [sh:134]
[   37.303671] CPU#2 Utilization every 4s during lockup:
[   37.303671] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   37.303671] 	#2:  12% system,	  0% softirq,	  0% hardirq,	 35% idle
[   37.303671] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   37.303671] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t12-d21/serial.log

[   30.290758] watchdog: BUG: soft lockup - CPU#0 stuck for 21s! [sh:134]
[   30.291259] CPU#0 Utilization every 4s during lockup:
[   30.291259] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.291259] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.291259] 	#3:  15% system,	  0% softirq,	  0% hardirq,	 45% idle
[   30.291259] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t13-d22/serial.log

[   36.967486] watchdog: BUG: soft lockup - CPU#2 stuck for 23s! [sh:133]
[   36.967676] CPU#2 Utilization every 4s during lockup:
[   36.967676] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.967676] 	#2:  21% system,	  0% softirq,	  0% hardirq,	 18% idle
[   36.967676] 	#3:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
[   36.967676] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-fine/tcg/t14-d23/serial.log

[   34.369841] watchdog: BUG: soft lockup - CPU#0 stuck for 25s! [sh:133]
[   34.369841] CPU#0 Utilization every 4s during lockup:
[   34.369841] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.369841] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.369841] 	#3:  18% system,	  0% softirq,	  0% hardirq,	 46% idle
[   34.369841] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-rcu/kvm/t00-d22/serial.log

[   31.295287] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:132]
[   31.295290] CPU#0 Utilization every 4s during lockup:
[   31.295291] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.295293] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.295293] 	#3:   3% system,	  0% softirq,	  0% hardirq,	 29% idle
[   31.295294] 	#4:   6% system,	  0% softirq,	  0% hardirq,	  0% idle

### RCU stall detector
runs/smp-rcu/kvm/t00-d22/serial.log

[   31.295270] rcu: INFO: rcu_sched self-detected stall on CPU
[   31.295287] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:132]
[   31.295290] CPU#0 Utilization every 4s during lockup:
[   31.295291] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.295293] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.295293] 	#3:   3% system,	  0% softirq,	  0% hardirq,	 29% idle

### soft-lockup detector
runs/smp-rcu/kvm/t01-d25/serial.log

[   35.517396] watchdog: BUG: soft lockup - CPU#0 stuck for 26s! [ls:11018]
[   35.517402] CPU#0 Utilization every 4s during lockup:
[   35.517403] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.517405] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.517405] 	#3:  11% system,	  0% softirq,	  0% hardirq,	 24% idle
[   35.517406] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### RCU stall detector
runs/smp-rcu/kvm/t01-d25/serial.log

[   35.517397] rcu: INFO: rcu_sched self-detected stall on CPU
[   35.517396] watchdog: BUG: soft lockup - CPU#0 stuck for 26s! [ls:11018]
[   35.517402] CPU#0 Utilization every 4s during lockup:
[   35.517403] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.517405] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.517405] 	#3:  11% system,	  0% softirq,	  0% hardirq,	 24% idle

### soft-lockup detector
runs/smp-rcu/kvm/t02-d30/serial.log

[   43.983409] watchdog: BUG: soft lockup - CPU#2 stuck for 29s! [sh:133]
[   43.983412] CPU#2 Utilization every 4s during lockup:
[   43.983413] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   43.983414] 	#2:   2% system,	  0% softirq,	  0% hardirq,	 22% idle
[   43.983415] 	#3:   2% system,	  0% softirq,	  0% hardirq,	  0% idle
[   43.983416] 	#4:   2% system,	  0% softirq,	  0% hardirq,	  0% idle

### RCU stall detector
runs/smp-rcu/kvm/t02-d30/serial.log

[   43.983398] rcu: INFO: rcu_sched self-detected stall on CPU
[   43.983409] watchdog: BUG: soft lockup - CPU#2 stuck for 29s! [sh:133]
[   43.983412] CPU#2 Utilization every 4s during lockup:
[   43.983413] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   43.983414] 	#2:   2% system,	  0% softirq,	  0% hardirq,	 22% idle
[   43.983415] 	#3:   2% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-rcu/kvm/t03-d45/serial.log

[   54.436567] watchdog: BUG: soft lockup - CPU#2 stuck for 43s! [sh:133]
[   54.436570] CPU#2 Utilization every 4s during lockup:
[   54.436571] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.436572] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.436573] 	#3:   9% system,	  0% softirq,	  0% hardirq,	 14% idle
[   54.436573] 	#4:   3% system,	  0% softirq,	  0% hardirq,	  0% idle

### RCU stall detector
runs/smp-rcu/kvm/t03-d45/serial.log

[   54.436556] rcu: INFO: rcu_sched detected stalls on CPUs/tasks:
[   54.436567] watchdog: BUG: soft lockup - CPU#2 stuck for 43s! [sh:133]
[   54.436570] CPU#2 Utilization every 4s during lockup:
[   54.436571] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.436572] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.436573] 	#3:   9% system,	  0% softirq,	  0% hardirq,	 14% idle

### soft-lockup detector
runs/smp-rcu/kvm/t04-d60/serial.log

[   73.643765] watchdog: BUG: soft lockup - CPU#2 stuck for 57s! [sh:129]
[   73.643767] rcu: INFO: rcu_sched detected stalls on CPUs/tasks:
[   73.643770] CPU#2 Utilization every 4s during lockup:
[   73.643772] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   73.643774] 	#2:  11% system,	  0% softirq,	  0% hardirq,	 14% idle
[   73.643776] 	#3:   2% system,	  0% softirq,	  0% hardirq,	  0% idle

### RCU stall detector
runs/smp-rcu/kvm/t04-d60/serial.log

[   73.643767] rcu: INFO: rcu_sched detected stalls on CPUs/tasks:
[   73.643770] CPU#2 Utilization every 4s during lockup:
[   73.643772] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   73.643774] 	#2:  11% system,	  0% softirq,	  0% hardirq,	 14% idle
[   73.643776] 	#3:   2% system,	  0% softirq,	  0% hardirq,	  0% idle
[   73.643776] 	#4:   3% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-rcu/kvm/t05-d125/serial.log

[  134.842535] watchdog: BUG: soft lockup - CPU#0 stuck for 118s! [sh:132]
[  134.842546] CPU#0 Utilization every 4s during lockup:
[  134.842546] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  134.842548] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  134.842549] 	#3:   4% system,	  0% softirq,	  0% hardirq,	 26% idle
[  134.842549] 	#4:   2% system,	  0% softirq,	  0% hardirq,	  1% idle

### RCU stall detector
runs/smp-rcu/kvm/t05-d125/serial.log

[  134.842533] rcu: INFO: rcu_sched self-detected stall on CPU
[  134.842535] watchdog: BUG: soft lockup - CPU#0 stuck for 118s! [sh:132]
[  134.842546] CPU#0 Utilization every 4s during lockup:
[  134.842546] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  134.842548] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  134.842549] 	#3:   4% system,	  0% softirq,	  0% hardirq,	 26% idle

### soft-lockup detector
runs/smp-sig/kvm/t04-d20/serial.log

[   30.972136] watchdog: BUG: soft lockup - CPU#1 stuck for 21s! [sh:128]
[   30.972142] CPU#1 Utilization every 4s during lockup:
[   30.972142] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.972144] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   30.972144] 	#3:   1% system,	  0% softirq,	  0% hardirq,	 24% idle
[   30.972145] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/kvm/t05-d22/serial.log

[   31.898006] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:134]
[   31.898011] CPU#0 Utilization every 4s during lockup:
[   31.898012] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.898018] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   31.898019] 	#3:   3% system,	  0% softirq,	  0% hardirq,	 24% idle
[   31.898020] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/kvm/t06-d25/serial.log

[   34.640517] watchdog: BUG: soft lockup - CPU#2 stuck for 25s! [sh:132]
[   34.640522] CPU#2 Utilization every 4s during lockup:
[   34.640523] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.640525] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   34.640525] 	#3:   8% system,	  0% softirq,	  0% hardirq,	 15% idle
[   34.640526] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/kvm/t07-d30/serial.log

[   38.942437] watchdog: BUG: soft lockup - CPU#2 stuck for 29s! [sh:129]
[   38.942447] CPU#2 Utilization every 4s during lockup:
[   38.942448] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   38.942449] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   38.942449] 	#3:   4% system,	  0% softirq,	  0% hardirq,	 18% idle
[   38.942450] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/kvm/t08-d45/serial.log

[   54.707591] watchdog: BUG: soft lockup - CPU#2 stuck for 43s! [sh:131]
[   54.707596] CPU#2 Utilization every 4s during lockup:
[   54.707597] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.707598] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   54.707599] 	#3:   1% system,	  0% softirq,	  0% hardirq,	 24% idle
[   54.707599] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/kvm/t09-d60/serial.log

[   73.257576] watchdog: BUG: soft lockup - CPU#2 stuck for 57s! [sh:131]
[   73.257581] CPU#2 Utilization every 4s during lockup:
[   73.257582] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   73.257583] 	#2:   4% system,	  0% softirq,	  0% hardirq,	 19% idle
[   73.257584] 	#3:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   73.257584] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/kvm/t10-d125/serial.log

[  133.763090] watchdog: BUG: soft lockup - CPU#3 stuck for 117s! [sh:131]
[  133.763095] CPU#3 Utilization every 4s during lockup:
[  133.763095] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  133.763097] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[  133.763098] 	#3:   6% system,	  0% softirq,	  0% hardirq,	 17% idle
[  133.763098] 	#4:   0% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/tcg/t04-d20/serial.log

[   32.191030] watchdog: BUG: soft lockup - CPU#0 stuck for 22s! [sh:135]
[   32.191030] CPU#0 Utilization every 4s during lockup:
[   32.191030] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.191030] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   32.191030] 	#3:  18% system,	  0% softirq,	  0% hardirq,	 32% idle
[   32.191030] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/tcg/t05-d22/serial.log

[   33.085870] watchdog: BUG: soft lockup - CPU#3 stuck for 22s! [sh:133]
[   33.085870] CPU#3 Utilization every 4s during lockup:
[   33.085870] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.085870] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   33.085870] 	#3:  23% system,	  0% softirq,	  0% hardirq,	 11% idle
[   33.085870] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/tcg/t06-d25/serial.log

[   35.817003] watchdog: BUG: soft lockup - CPU#0 stuck for 26s! [sh:133]
[   35.817003] CPU#0 Utilization every 4s during lockup:
[   35.817003] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.817003] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   35.817003] 	#3:  14% system,	  0% softirq,	  0% hardirq,	 45% idle
[   35.817003] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/tcg/t07-d30/serial.log

[   40.421894] watchdog: BUG: soft lockup - CPU#0 stuck for 30s! [sh:133]
[   40.421894] CPU#0 Utilization every 4s during lockup:
[   40.421894] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   40.421894] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   40.421894] 	#3:  23% system,	  0% softirq,	  0% hardirq,	 32% idle
[   40.421894] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle

### soft-lockup detector
runs/smp-sig/tcg/t10-d125/serial.log

[   49.442166] watchdog: BUG: soft lockup - CPU#2 stuck for 38s! [sh:134]
[   49.442477] CPU#2 Utilization every 4s during lockup:
[   49.442477] 	#1:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   49.442477] 	#2:   0% system,	  0% softirq,	  0% hardirq,	  0% idle
[   49.442477] 	#3:  20% system,	  0% softirq,	  0% hardirq,	 18% idle
[   49.442477] 	#4:   1% system,	  0% softirq,	  0% hardirq,	  0% idle
