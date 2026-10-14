#pragma once

#include <string>
#include <vector>

#include "cube.hpp"

// `jn --describe <inputs>`: how the inputs open (the cube, or the variables of
// a container), without a window; returns the exit code.
int describeInputs(const std::vector<std::string>& inputs, const BandSelection& sel);

// Runs the Zeit integration end to end without a window; returns the exit code.
// `threads`: CPU threads of the bridge processes (as the Settings value).
int runZeitSelfTest(const std::vector<std::string>& inputs, const BandSelection& sel, const std::string& python,
                    const std::string& bridge, int threads);
