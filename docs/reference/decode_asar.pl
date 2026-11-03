#!/usr/bin/perl
# Decoder dei blob parametri DAP-VR ("ASAR") che Dolby Access scrive nel property store
# dell'endpoint audio, chiave di registro:
#   HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{endpoint}\
#     FxProperties\{45da5c30-2837-4ac4-b1e2-50acc3865974}\User
#   valori {1b4dab55-b1fb-4d8c-8317-f2d4a96efbb8},2 (init-time) e ,5 (runtime)
# Formato: header PROPVARIANT di 8 byte (41 00 00 00 = VT_BLOB, 01 00 00 00), poi per ogni
# campo 1 byte di presenza (0 = null, 1 = presente) + valore: bool 1 byte, int32 LE,
# array = int32 conteggio + elementi. I guadagni sono in 1/16 di dB.
# L'ordine dei campi e' quello del DataContract dell'app (ricavato dal binario).
# Uso: perl decode_asar.pl file1.bin file2.bin ...
use strict; use warnings;
my @schema = (
 ['output_mode','i'],['mix_matrix','ints'],['pregain','i'],['postgain','i'],['system_gain','i'],['calibration_boost','i'],
 ['process_optimizer_enable','b'],['process_optimizer_config','pairs'],['surround_decoder_enable','b'],
 ['virtualizer_headphone_late_gain','i'],['virtualizer_front_speaker_angle','i'],['virtualizer_surround_speaker_angle','i'],
 ['virtualizer_height_speaker_angle','i'],['surround_boost','i'],['mi2ieq_steering_enable','b'],['mi2dv_leveler_steering_enable','b'],
 ['mi2dialog_enhancer_steering_enable','b'],['mi2surround_compressor_steering_enable','b'],['volume_leveler_amount','i'],
 ['volume_leveler_in_target','i'],['volume_leveler_out_target','i'],['volume_leveler_enable','b'],['volume_modeler_calibration','i'],
 ['volume_modeler_enable','b'],['ieq_bands','pairs'],['ieq_enable','b'],['ieq_amount','i'],['de_enable','b'],['de_amount','i'],['de_ducking','i'],
 ['volmax_boost','i'],['graphic_equalizer_enable','b'],['graphic_equalizer_config','pairs'],['audio_optimizer_enable','b'],
 ['audio_optimizer_config','pairs'],['height_filter_mode','i'],['bass_enhancer_enable','b'],['bass_enhancer_boost','i'],
 ['bass_enhancer_cutoff_frequency','i'],['bass_enhancer_width','i'],['bass_extraction_enable','b'],['bass_extraction_cutoff_frequency','i'],
 ['regulator_tuning','pairs'],['regulator_overdrive','i'],['regulator_timbre_preservation','i'],['regulator_relaxation_amount','i'],
 ['regulator_speaker_distortion_enable','b'],['regulator_enable','b'],['channel_count','i'],['stereo_cp_bypass_mode','i'],
 ['stereo_bypass_dap_dll','b'],['volume_leveler_drc_enable','b'],['eyes_tracking_enable','b'],['speaker_virtualizer_mode','i'],
 ['speaker_virtualizer_start_bin','i'],['volume_leveler_compressor_enable','b'],['output_mode_v2_deprecated','i'],
 ['mi2adaptive_virtualizer_steering_enable','b'],['virtual_bass_mode','i'],['virtual_bass_src_freqs','ints'],['virtual_bass_overall_gain','i'],
 ['virtual_bass_slope_gain','i'],['virtual_bass_subgains','ints'],['virtual_bass_mix_freqs','ints'],['speaker_virtualizer_rendering_config','ints'],
 ['dynamic_speaker_optimization_amount','i'],['dynamic_speaker_optimization_speaker_interval','ints'],['z_distance','i'],
);
for my $file (@ARGV) {
  local $/; open(my $f,'<:raw',$file) or die "$file: $!"; my $d=<$f>; close $f;
  (my $name = $file) =~ s{.*/}{};
  print "\n=== $name (", length($d), " B) ===\n";
  my $p = 8; # skip 41000000 01000000 header
  for my $fld (@schema) {
    my ($n,$t) = @$fld; last if $p >= length($d);
    my $flag = ord(substr($d,$p,1)); $p++;
    if ($flag == 0) { print "  $n: (null)\n"; next; }
    if ($flag != 1) { print "  $n: UNEXPECTED flag $flag at ", $p-1, " -- stopping\n"; last; }
    if ($t eq 'b') { my $v = ord(substr($d,$p,1)); $p++; print "  $n: ", ($v ? "true":"false"), "\n"; }
    elsif ($t eq 'i') { my $v = unpack('l<', substr($d,$p,4)); $p+=4; my $db = ($n =~ /gain|boost|target|amount|threshold/) ? sprintf(" (%.2f dB if 1/16 dB)", $v/16) : ""; print "  $n: $v$db\n"; }
    elsif ($t eq 'ints') { my $c = unpack('l<', substr($d,$p,4)); $p+=4; my @v = unpack("l<$c", substr($d,$p,4*$c)); $p+=4*$c; print "  $n: [$c] ", join(', ',@v), "\n"; }
    elsif ($t eq 'pairs') { my $c = unpack('l<', substr($d,$p,4)); $p+=4; my @v = unpack("l<".(2*$c), substr($d,$p,8*$c)); $p+=8*$c; my @s; for (my $i=0;$i<$c;$i++){ push @s, "$v[2*$i]Hz:".sprintf("%.1f",$v[2*$i+1]/16) } print "  $n: [$c] ", join(' ',@s), "\n"; }
  }
  print "  -- consumed $p of ", length($d), " bytes\n";
}
