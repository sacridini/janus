#pragma once

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <mutex>
#include <queue>
#include <thread>
#include <vector>

// Thread pool with priorities. Lower `priority` runs first; within the same
// priority the newest job runs first (LIFO), because for tiles and series the
// most recent request is the one the user cares about.
class JobPool {
public:
    explicit JobPool(int threads, std::function<void()> onJobDone = {})
        : onJobDone_(std::move(onJobDone)) {
        setThreads(threads);
    }
    ~JobPool() {
        {
            std::lock_guard<std::mutex> lk(m_);
            stop_ = true;
            while (!q_.empty()) q_.pop();
        }
        cv_.notify_all();
        for (auto& t : workers_) t.join();
    }
    JobPool(const JobPool&) = delete;
    JobPool& operator=(const JobPool&) = delete;

    void submit(int priority, std::function<void()> fn) {
        bool parked = false;
        {
            std::lock_guard<std::mutex> lk(m_);
            q_.push(Job{priority, seq_++, std::move(fn)});
            ++pending_;
            parked = limit_ < int(workers_.size());
        }
        // A parked worker (beyond the limit) would take the wake-up and go back to sleep.
        if (parked) cv_.notify_all();
        else cv_.notify_one();
    }
    // Workers allowed to run jobs (the processing threads setting): more are
    // started when it grows; when it shrinks, the extra ones finish their job
    // and park.
    void setThreads(int n) {
        {
            std::lock_guard<std::mutex> lk(m_);
            limit_ = std::max(1, n);
            for (int i = int(workers_.size()); i < limit_; ++i) workers_.emplace_back([this, i] { loop(i); });
        }
        cv_.notify_all();
    }
    // Queued + running jobs.
    int pending() const { return pending_.load(); }
    int threads() const { return limit_.load(); }

private:
    struct Job {
        int priority;
        uint64_t seq;
        std::function<void()> fn;
        bool operator<(const Job& o) const {
            if (priority != o.priority) return priority > o.priority;
            return seq < o.seq;
        }
    };

    void loop(int index) {
        for (;;) {
            Job job;
            {
                std::unique_lock<std::mutex> lk(m_);
                cv_.wait(lk, [this, index] { return stop_ || (index < limit_ && !q_.empty()); });
                if (stop_) return;
                job = std::move(const_cast<Job&>(q_.top()));
                q_.pop();
            }
            job.fn();
            --pending_;
            if (onJobDone_) onJobDone_();
        }
    }

    std::vector<std::thread> workers_;
    std::priority_queue<Job> q_;
    std::mutex m_;
    std::condition_variable cv_;
    bool stop_ = false;
    uint64_t seq_ = 0;
    std::atomic<int> pending_{0};
    std::atomic<int> limit_{0};
    std::function<void()> onJobDone_;
};
