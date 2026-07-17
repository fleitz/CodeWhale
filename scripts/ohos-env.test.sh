#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp_base="${CODEWHALE_TEST_TMPDIR:-${TMPDIR:-/tmp}}"
mkdir -p "${tmp_base}"
tmp_dir="$(mktemp -d "${tmp_base%/}/codewhale-ohos-env.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

assert_equal() {
  local name="$1"
  local actual="$2"
  local expected="$3"

  if [[ "${actual}" != "${expected}" ]]; then
    printf '%s mismatch\nexpected: %q\nactual:   %q\n' \
      "${name}" "${expected}" "${actual}" >&2
    exit 1
  fi
}

if env -u OHOS_NATIVE_SDK sh "${repo_root}/scripts/ohos-env.sh" \
  >"${tmp_dir}/unset.log" 2>&1; then
  echo "ohos-env.sh unexpectedly accepted an unset OHOS_NATIVE_SDK" >&2
  exit 1
fi
grep -Fq 'set OHOS_NATIVE_SDK' "${tmp_dir}/unset.log"

sdk="${tmp_dir}/OpenHarmony Native SDK"
mkdir -p \
  "${sdk}/llvm/bin" \
  "${sdk}/sysroot" \
  "${sdk}/build/cmake"
touch \
  "${sdk}/llvm/bin/clang" \
  "${sdk}/llvm/bin/clang++" \
  "${sdk}/llvm/bin/llvm-ar" \
  "${sdk}/build/cmake/ohos.toolchain.cmake"

(
  export OHOS_NATIVE_SDK="${sdk}"
  # shellcheck source=scripts/ohos-env.sh
  . "${repo_root}/scripts/ohos-env.sh" >/dev/null

  resolved_sdk="$(cd "${sdk}" && pwd)"
  sysroot="${resolved_sdk}/sysroot"
  common_flags="-target aarch64-linux-ohos --sysroot=\"${sysroot}\" -D__MUSL__"
  separator="$(printf '\037')"
  encoded_rustflags="-Clink-arg=-target${separator}-Clink-arg=aarch64-linux-ohos${separator}-Clink-arg=--sysroot=${sysroot}${separator}-Clink-arg=-D__MUSL__"

  assert_equal \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER \
    "${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER}" \
    "${resolved_sdk}/llvm/bin/clang"
  assert_equal \
    AR_aarch64_unknown_linux_ohos \
    "${AR_aarch64_unknown_linux_ohos}" \
    "${resolved_sdk}/llvm/bin/llvm-ar"
  assert_equal \
    CC_aarch64_unknown_linux_ohos \
    "${CC_aarch64_unknown_linux_ohos}" \
    "${resolved_sdk}/llvm/bin/clang"
  assert_equal \
    CXX_aarch64_unknown_linux_ohos \
    "${CXX_aarch64_unknown_linux_ohos}" \
    "${resolved_sdk}/llvm/bin/clang++"
  assert_equal CFLAGS_aarch64_unknown_linux_ohos \
    "${CFLAGS_aarch64_unknown_linux_ohos}" "${common_flags}"
  assert_equal CXXFLAGS_aarch64_unknown_linux_ohos \
    "${CXXFLAGS_aarch64_unknown_linux_ohos}" "${common_flags}"
  assert_equal BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_ohos \
    "${BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_ohos}" "${common_flags}"
  assert_equal CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos \
    "${CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos}" \
    "${resolved_sdk}/build/cmake/ohos.toolchain.cmake"
  assert_equal CC_SHELL_ESCAPED_FLAGS "${CC_SHELL_ESCAPED_FLAGS}" "1"
  assert_equal CARGO_ENCODED_RUSTFLAGS \
    "${CARGO_ENCODED_RUSTFLAGS}" "${encoded_rustflags}"
)

echo "ohos-env.sh tests passed"
