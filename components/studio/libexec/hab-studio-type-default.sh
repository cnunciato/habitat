studio_type="default"
studio_path="$BLDR_ROOT/bin"
studio_enter_environment=
studio_enter_command="$BLDR_ROOT/bin/hab-bpm exec chef/hab-backline bash --login +h"
studio_build_environment=
studio_build_command="record \${1:-} $BLDR_ROOT/bin/build"
studio_run_environment=
studio_run_command="$BLDR_ROOT/bin/hab-bpm exec chef/hab-backline bash -l"

pkgs="chef/hab-bpm chef/hab-backline chef/hab-studio"

finish_setup() {
  if [ -x "$STUDIO_ROOT$BLDR_ROOT/bin/hab-bpm" ]; then
    return 0
  fi

  for pkg in $pkgs; do
    _bpm install $pkg
  done

  local bpm_path=$(_pkgpath_for chef/hab-bpm)
  local bash_path=$(_pkgpath_for chef/bash)
  local coreutils_path=$(_pkgpath_for chef/coreutils)

  $bb mkdir -p $v $STUDIO_ROOT$BLDR_ROOT/bin

  # Put `hab-bpm` on the default `$PATH` and ensure that it gets a sane shell
  # and initial `busybox` (sane being its own vendored version)
  $bb cat <<EOF > $STUDIO_ROOT$BLDR_ROOT/bin/hab-bpm
#!$bpm_path/libexec/busybox sh
export BUSYBOX=$bpm_path/libexec/busybox
exec \$BUSYBOX sh $bpm_path/bin/hab-bpm \$*
EOF
  $bb chmod $v 755 $STUDIO_ROOT$BLDR_ROOT/bin/hab-bpm

  # Create a wrapper to `build` so that any calls to it have a super-stripped
  # `$PATH` and not whatever augmented version is currently in use. This should
  # mean that running `build` from inside a `studio enter` and running `studio
  # build` leads to the exact same experience, at least as far as initial
  # `$PATH` is concerned.
  $bb cat <<EOF > $STUDIO_ROOT$BLDR_ROOT/bin/build
#!$bpm_path/libexec/busybox sh
exec $BLDR_ROOT/bin/hab-bpm exec chef/hab-plan-build hab-plan-build \$*
EOF
  $bb chmod $v 755 $STUDIO_ROOT$BLDR_ROOT/bin/build

  # Create a wrapper to studio
  $bb cat <<EOF > $STUDIO_ROOT$BLDR_ROOT/bin/studio
#!$bpm_path/libexec/busybox sh
exec $BLDR_ROOT/bin/hab-bpm exec chef/hab-studio hab-studio \$*
EOF
  $bb chmod $v 755 $STUDIO_ROOT$BLDR_ROOT/bin/studio

  $bb ln -s $v $bash_path/bin/bash $STUDIO_ROOT/bin/bash
  $bb ln -s $v bash $STUDIO_ROOT/bin/sh

  # Set the login shell for any relevant user to be `/bin/bash`
  $bb sed -e "s,/bin/sh,$bash_path/bin/bash,g" -i $STUDIO_ROOT/etc/passwd

  $bb cat >> $STUDIO_ROOT/etc/profile <<PROFILE
# Add hab-bpm to the default PATH at the front so any wrapping scripts will
# be found and called first
export PATH=$BLDR_ROOT/bin:\$PATH

# Colorize grep/egrep/fgrep by default
alias grep='grep --color=auto'
alias egrep='egrep --color=auto'
alias fgrep='fgrep --color=auto'

PROFILE

  studio_env_command="$coreutils_path/bin/env"
}

_bpm() {
  $bb env BUSYBOX=$bb FS_ROOT=$STUDIO_ROOT $bb sh $bpm $*
}

_pkgpath_for() {
  _bpm pkgpath $1 | $bb sed -e "s,^$STUDIO_ROOT,,g"
}