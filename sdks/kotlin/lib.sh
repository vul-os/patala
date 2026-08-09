#!/usr/bin/env bash
#
# lib.sh — toolchain discovery shared by run-examples.sh and
# uniffi-kotlin-probe.sh. Not executable on its own; source it.
#
# Everything here exists because the JVM/Kotlin toolchain is found three
# different ways on the three machines this has run on, and a script that
# guesses wrong reports "java is not on PATH" for a machine that has four
# JDKs. Each function tries every plausible location, VERIFIES the thing it
# found actually runs, and on failure prints every path it tried.
#
# In particular: macOS ships a /usr/bin/java STUB that exists, is executable,
# and fails with "Unable to locate a Java Runtime" when no JDK is installed.
# `command -v java` is therefore not a JDK check, and every function below
# runs the candidate before believing it.

# Print the bin directory of a working JDK, or fail with the list of
# candidates. Order: $JAVA_HOME, macOS java_home, Homebrew, PATH.
patala_find_jdk_bin() {
  local candidates=() c
  [ -n "${JAVA_HOME:-}" ] && candidates+=("${JAVA_HOME}/bin")
  if [ -x /usr/libexec/java_home ]; then
    local jh
    jh="$(/usr/libexec/java_home 2>/dev/null || true)"
    [ -n "${jh}" ] && candidates+=("${jh}/bin")
  fi
  if command -v brew >/dev/null 2>&1; then
    local prefix
    for formula in openjdk openjdk@26 openjdk@25 openjdk@24 openjdk@23 openjdk@22; do
      prefix="$(brew --prefix "${formula}" 2>/dev/null || true)"
      [ -n "${prefix}" ] && [ -x "${prefix}/bin/java" ] && candidates+=("${prefix}/bin")
    done
  fi
  local onpath
  onpath="$(command -v java 2>/dev/null || true)"
  [ -n "${onpath}" ] && candidates+=("$(dirname "${onpath}")")

  for c in "${candidates[@]}"; do
    # Runs it: /usr/bin/java on a Mac with no JDK exists and does not work.
    if [ -x "${c}/java" ] && "${c}/java" -version >/dev/null 2>&1; then
      echo "${c}"
      return 0
    fi
  done
  printf 'patala: FAIL — no working JDK found. Tried:\n' >&2
  printf '  %s\n' "${candidates[@]:-(nothing)}" >&2
  printf 'Install one (brew install openjdk) or set JAVA_HOME.\n' >&2
  return 1
}

# Print the JDK's major version (e.g. 26). Assumes java is on PATH.
patala_jdk_major() {
  java -XshowSettings:properties -version 2>&1 \
    | sed -n 's/^ *java\.specification\.version *= *//p' | cut -d. -f1
}

# Print the path to kotlin-stdlib.jar, or fail with the list of candidates.
#
# kotlinc puts it on the COMPILE classpath only, so running a compiled program
# needs it explicitly. Finding it means resolving through however kotlinc was
# installed — a Homebrew symlink, an SDKMAN shim, or a plain unpacked
# distribution.
patala_find_kotlin_stdlib() {
  local candidates=() c bin real target
  [ -n "${KOTLIN_HOME:-}" ] && candidates+=("${KOTLIN_HOME}/lib/kotlin-stdlib.jar")
  bin="$(command -v kotlinc)" || return 1
  real="${bin}"
  # Follow the symlink chain by hand: `readlink -f` is GNU and absent on some
  # macOS versions, and a missing tool here would look like a missing jar.
  while [ -L "${real}" ]; do
    target="$(readlink "${real}")"
    case "${target}" in
      /*) real="${target}" ;;
      *)  real="$(dirname "${real}")/${target}" ;;
    esac
  done
  for c in "$(dirname "$(dirname "${real}")")" "$(dirname "$(dirname "${bin}")")"; do
    candidates+=("${c}/lib/kotlin-stdlib.jar" "${c}/libexec/lib/kotlin-stdlib.jar")
  done
  if command -v brew >/dev/null 2>&1; then
    candidates+=("$(brew --prefix kotlin 2>/dev/null)/libexec/lib/kotlin-stdlib.jar")
  fi
  for c in "${candidates[@]}"; do
    if [ -f "${c}" ]; then echo "${c}"; return 0; fi
  done
  printf 'patala: FAIL — could not find kotlin-stdlib.jar. Tried:\n' >&2
  printf '  %s\n' "${candidates[@]}" >&2
  printf 'Set KOTLIN_HOME to the Kotlin distribution root.\n' >&2
  return 1
}

# Print the path to the JNA jar at the PINNED version, or fail with the one
# command that installs it.
#
# The generated UniFFI Kotlin is a `com.sun.jna.Library`; there is no
# generated-Kotlin build without this jar. $PATALA_JNA_JAR overrides, and the
# version is pinned so a machine with three JNAs in ~/.m2 compiles against the
# same one CI does.
patala_find_jna() {
  local version="${1:?patala_find_jna: version required}"
  if [ -n "${PATALA_JNA_JAR:-}" ]; then
    [ -f "${PATALA_JNA_JAR}" ] || {
      echo "patala: FAIL — PATALA_JNA_JAR=${PATALA_JNA_JAR} does not exist" >&2
      return 1
    }
    echo "${PATALA_JNA_JAR}"
    return 0
  fi
  local jar="${HOME}/.m2/repository/net/java/dev/jna/jna/${version}/jna-${version}.jar"
  if [ ! -f "${jar}" ] && command -v mvn >/dev/null 2>&1; then
    echo "patala: fetching net.java.dev.jna:jna:${version}…" >&2
    mvn -q dependency:get -Dartifact="net.java.dev.jna:jna:${version}" >/dev/null 2>&1 || true
  fi
  if [ -f "${jar}" ]; then echo "${jar}"; return 0; fi
  cat >&2 <<EOF
patala: FAIL — no JNA ${version} jar at
  ${jar}
The generated UniFFI Kotlin imports com.sun.jna and cannot compile without it.
Install it with:
  mvn dependency:get -Dartifact=net.java.dev.jna:jna:${version}
or point PATALA_JNA_JAR at a jar you already have.
EOF
  return 1
}
