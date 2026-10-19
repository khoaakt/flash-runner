#!/usr/bin/perl
# usage: with-timeout.pl SECONDS command args...   (macOS has no timeout(1)); the command is killed by SIGALRM after SECONDS
use strict;
my $secs = shift @ARGV;
alarm $secs;
exec @ARGV or die "exec failed: $!";
