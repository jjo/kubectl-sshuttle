# vhs tape — kubectl-sshuttle cover demo
# Render: vhs kubectl-sshuttle-cover.tape
# Output: kubectl-sshuttle-cover.gif (+ .mp4 if Output line set)

Output kubectl-sshuttle-cover.gif
Output kubectl-sshuttle-cover.mp4

Set Shell bash
Set FontSize 18
Set Width 1280
Set Height 720
Set Theme "Catppuccin Mocha"
#Set FontFamily "Fira Mono"
Set TypingSpeed 35ms
Set PlaybackSpeed 1.0
Set Padding 30
Set Margin 20
Set MarginFill "#1e1e2e"

# Bootstrap shell env + switch context (offstage)
Hide
Type "source ~/.bashrc.jjo && ktx um-metal.config"
Enter
Wait /[\$\>]/
Sleep 300ms
Type "clear"
Enter
Wait /[\$\>]/
Sleep 200ms
Show

# Title card
Type "# kubectl sshuttle — cluster (and beyond!) network on your laptop"
Enter
Wait /[\$\>]/
Sleep 1500ms

# Step 1: deploy proxy pod (fast cut)
Type "# Step 1: deploy sshuttle proxy pod"
Enter
Wait /[\$\>]/
Type "kubectl sshuttle create"
Sleep 600ms
Enter
Wait /[\$\>]/
Sleep 1500ms
Type "kubectl sshuttle status"
Sleep 600ms
Enter
Wait /[\$\>]/
Sleep 3500ms
#Type "clear"
#Enter

# Step 2: connect — full ClusterIP range + DNS
Type "# Step 2: connect — full ClusterIP range + DNS"
Enter
Wait /[\$\>]/
Type "kubectl sshuttle connect 10.43.0.0/16 -- --dns &"
Enter
Sleep 5s
Enter
Wait /[\$\>]/
Sleep 2s
#Type "clear"
#Enter

# Step 3: ClusterIP reachable from laptop
Type "# Step 3: pick some ClusterIP to reach from your laptop, e.g. from Rook deploy"
Enter
Wait /[\$\>]/
Type "kubectl get svc -n rook-ceph"
Sleep 400ms
Enter
Wait /[\$\>]/
Sleep 2500ms

# Step 4: the magic — DNS by service name
Type "# Step4: now the magic: reach some svc endpoint by *name* ✨ ..."
Sleep 1200ms
Enter
Wait /[\$\>]/

Type "curl -sLk http://rook-ceph-mgr.rook-ceph.svc.cluster.local:9283/metrics | egrep ^ceph_ | shuf -n3"
Sleep 400ms
Enter
Wait /[\$\>]/
Sleep 4500ms

Type "# ... note again the .svc.cluster.local suffix!"
Sleep 1200ms
Enter
Type "curl -sLk https://rook-ceph-mgr-dashboard.rook-ceph.svc.cluster.local:8443/ |& grep title"
Sleep 400ms
Enter
Wait /[\$\>]/
Sleep 3500ms

# Closing card
Type "# kubectl krew install sshuttle"
Sleep 2500ms
Enter
Sleep 1500ms
