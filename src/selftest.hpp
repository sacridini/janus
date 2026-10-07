#pragma once

#include <string>
#include <vector>

// Runs the Zeit integration end to end without a window; returns the exit code.
int runZeitSelfTest(const std::vector<std::string>& inputs, int band, const std::string& python,
                    const std::string& bridge);
