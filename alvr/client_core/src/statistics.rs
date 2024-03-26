use alvr_common::SlidingWindowAverage;
use alvr_packets::ClientStatistics;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::connection::VideoStatsRx;

#[derive(Clone)]
struct HistoryFrame {
    input_acquired: Instant,
    video_packet_received: Instant,
    client_stats: ClientStatistics,
}

pub struct StatisticsManager {
    history_buffer: VecDeque<HistoryFrame>,
    max_history_size: usize,
    prev_vsync: Instant,
    total_pipeline_latency_average: SlidingWindowAverage<Duration>,
    steamvr_pipeline_latency: Duration,

    stats_history_buffer: VecDeque<HistoryFrame>, 
}

impl StatisticsManager {
    pub fn new(
        max_history_size: usize,
        nominal_server_frame_interval: Duration,
        steamvr_pipeline_frames: f32,
    ) -> Self {
        Self {
            max_history_size,
            history_buffer: VecDeque::new(),
            prev_vsync: Instant::now(),
            total_pipeline_latency_average: SlidingWindowAverage::new(
                Duration::ZERO,
                max_history_size,
            ),
            steamvr_pipeline_latency: Duration::from_secs_f32(
                steamvr_pipeline_frames * nominal_server_frame_interval.as_secs_f32(),
            ),
            stats_history_buffer: VecDeque::new(), 
        }
    }

    pub fn report_input_acquired(&mut self, target_timestamp: Duration) {
        if !self
            .history_buffer
            .iter()
            .any(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            self.history_buffer.push_front(HistoryFrame {
                input_acquired: Instant::now(),
                // this is just a placeholder because Instant does not have a default value
                video_packet_received: Instant::now(),
                client_stats: ClientStatistics {
                    target_timestamp,
                    ..Default::default()
                },
            });
        }

        if self.history_buffer.len() > self.max_history_size {
            self.history_buffer.pop_back();
        }
    }

    pub fn report_video_packet_received(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.video_packet_received = Instant::now();
        }
    }

    pub fn report_video_statistics(&mut self, target_timestamp: Duration, video_stats: VideoStatsRx)
    {
        if let Some(frame) = self
        .history_buffer
        .iter_mut()
        .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.client_stats.jitter_avg_frame = video_stats.jitter_avg_frame; 
            frame.client_stats.frame_span = video_stats.frame_span; 
            frame.client_stats.frame_interarrival = video_stats.frame_interarrival;
            frame.client_stats.rx_bytes = video_stats.rx_bytes;      
            frame.client_stats.bytes_in_frame = video_stats.bytes_in_frame;  
            frame.client_stats.bytes_in_frame_app = video_stats.bytes_in_frame_app; 
            frame.client_stats.filtered_ow_delay = video_stats.filtered_ow_delay; 

            frame.client_stats.rx_shard_counter = video_stats.rx_shard_counter; 
            frame.client_stats.duplicated_shard_counter = video_stats.duplicated_shard_counter; 
            frame.client_stats.highest_rx_frame_index = video_stats.highest_rx_frame_index; 
            frame.client_stats.highest_rx_shard_index = video_stats.highest_rx_shard_index; 
            frame.client_stats.frames_skipped = video_stats.frames_skipped; 
            frame.client_stats.frames_dropped = video_stats.frames_dropped;

            self.stats_history_buffer.push_back(frame.clone());
        }
    }
    pub fn report_frame_decoded(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.client_stats.video_decode =
                Instant::now().saturating_duration_since(frame.video_packet_received);
        }
    }

    pub fn report_compositor_start(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.client_stats.video_decoder_queue = Instant::now().saturating_duration_since(
                frame.video_packet_received + frame.client_stats.video_decode,
            );
        }
    }

    // vsync_queue is the latency between this call and the vsync. it cannot be measured by ALVR and
    // should be reported by the VR runtime
    pub fn report_submit(&mut self, target_timestamp: Duration, vsync_queue: Duration) {
        let now = Instant::now();

        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.client_stats.target_timestamp == target_timestamp)
        {
            frame.client_stats.rendering = now.saturating_duration_since(
                frame.video_packet_received
                    + frame.client_stats.video_decode
                    + frame.client_stats.video_decoder_queue,
            );
            frame.client_stats.vsync_queue = vsync_queue;
            frame.client_stats.total_pipeline_latency =
                now.saturating_duration_since(frame.input_acquired) + vsync_queue;
            self.total_pipeline_latency_average
                .submit_sample(frame.client_stats.total_pipeline_latency);

            let vsync = now + vsync_queue;
            frame.client_stats.frame_interval = vsync.saturating_duration_since(self.prev_vsync);
            self.prev_vsync = vsync;
        }
    }

    pub fn summary(&mut self, target_timestamp: Duration) -> Option<ClientStatistics> {
        if let Some(index) = self.stats_history_buffer
            .iter()
            .position(|frame| frame.client_stats.target_timestamp == target_timestamp){
                if let Some(frame) = self.stats_history_buffer.remove(index) {
                    Some(frame.client_stats)
                } else {
                    None
                }
            } else {
                None
            }
    }

    // latency used for head prediction
    pub fn average_total_pipeline_latency(&self) -> Duration {
        self.total_pipeline_latency_average.get_average()
    }

    // latency used for controllers/trackers prediction
    pub fn tracker_prediction_offset(&self) -> Duration {
        self.total_pipeline_latency_average
            .get_average()
            .saturating_sub(self.steamvr_pipeline_latency)
    }
}
