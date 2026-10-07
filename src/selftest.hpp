#pragma once

#include <string>
#include <vector>

#include "cube.hpp"

// Runs the Zeit integration end to end without a window; returns the exit code.
int runZeitSelfTest(const std::vector<std::string>& inputs, const BandSelection& sel, const std::string& python,
                    const std::string& bridge);
