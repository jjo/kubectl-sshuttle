#!/bin/bash -x
ffmpeg -i kubectl-sshuttle-cover.mp4 -vf "scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2:#1e1e2e" -loop 0 kubectl-sshuttle-cover.webp
ffmpeg -ss 00:00:55 -i kubectl-sshuttle-cover.mp4 -frames:v 1 -vf "scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2:#1e1e2e" kubectl-sshuttle-cover-static.webp
