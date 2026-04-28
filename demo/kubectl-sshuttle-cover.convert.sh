#!/bin/bash -x
ffmpeg -i kubectl-sshuttle-cover.mp4 -vf "scale=1600:900:force_original_aspect_ratio=decrease,pad=1600:900:(ow-iw)/2:(oh-ih)/2:#1e1e2e" -loop 0 kubectl-sshuttle-cover.webp
