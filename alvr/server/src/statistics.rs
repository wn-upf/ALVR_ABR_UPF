use alvr_common::{SlidingWindowAverage, HEAD_ID};
use alvr_events::{EventType, GraphStatistics, NominalBitrateStats, StatisticsSummary};
use alvr_packets::ClientStatistics;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

const FULL_REPORT_INTERVAL: Duration = Duration::from_millis(500);

pub struct HistoryFrame {
    target_timestamp: Duration,

    tracking_received: Instant,
    frame_present: Instant,
    frame_composed: Instant,
    frame_encoded: Instant,
    video_packet_bytes: usize,
    total_pipeline_latency: Duration,

    frame_index: u32,
    is_idr: bool,
}

impl Default for HistoryFrame {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            target_timestamp: Duration::ZERO,
            tracking_received: now,
            frame_present: now,
            frame_composed: now,
            frame_encoded: now,
            video_packet_bytes: 0,
            total_pipeline_latency: Duration::ZERO,
            frame_index: 0,
            is_idr: false,
        }
    }
}

#[derive(Default, Clone)]
struct BatteryData {
    gauge_value: f32,
    is_plugged: bool,
}

pub struct StatisticsManager {
    history_buffer: VecDeque<HistoryFrame>,
    max_history_size: usize,
    last_full_report_instant: Instant,
    last_frame_present_instant: Instant,
    last_frame_present_interval: Duration,
    video_packets_total: usize,
    video_packets_partial_sum: usize,
    video_bytes_total: usize,
    video_bytes_partial_sum: usize,
    packets_lost_total: usize,
    packets_lost_partial_sum: usize,
    battery_gauges: HashMap<u64, BatteryData>,
    steamvr_pipeline_latency: Duration,
    total_pipeline_latency_average: SlidingWindowAverage<Duration>,
    last_vsync_time: Instant,
    frame_interval: Duration,
    last_nominal_bitrate_stats: NominalBitrateStats,

    map_frames_spf: HashMap<u32, usize>,
    prev_highest_shard: i32,
    prev_highest_frame: i32, 
}

impl StatisticsManager {
    // history size used to calculate average total pipeline latency
    pub fn new(
        max_history_size: usize,
        nominal_server_frame_interval: Duration,
        steamvr_pipeline_frames: f32,
    ) -> Self {
        Self {
            history_buffer: VecDeque::new(),
            max_history_size,
            last_full_report_instant: Instant::now(),
            last_frame_present_instant: Instant::now(),
            last_frame_present_interval: Duration::ZERO,
            video_packets_total: 0,
            video_packets_partial_sum: 0,
            video_bytes_total: 0,
            video_bytes_partial_sum: 0,
            packets_lost_total: 0,
            packets_lost_partial_sum: 0,
            battery_gauges: HashMap::new(),
            steamvr_pipeline_latency: Duration::from_secs_f32(
                steamvr_pipeline_frames * nominal_server_frame_interval.as_secs_f32(),
            ),
            total_pipeline_latency_average: SlidingWindowAverage::new(
                Duration::ZERO,
                max_history_size,
            ),
            last_vsync_time: Instant::now(),
            frame_interval: nominal_server_frame_interval,
            last_nominal_bitrate_stats: NominalBitrateStats::default(),
            
            map_frames_spf: HashMap::new(), 
            prev_highest_shard: -1,
            prev_highest_frame: -1, 
        }
    }

    pub fn report_tracking_received(&mut self, target_timestamp: Duration) {
        if !self
            .history_buffer
            .iter()
            .any(|frame| frame.target_timestamp == target_timestamp)
        {
            self.history_buffer.push_front(HistoryFrame {
                target_timestamp,
                tracking_received: Instant::now(),
                ..Default::default()
            });
        }

        if self.history_buffer.len() > self.max_history_size {
            self.history_buffer.pop_back();
        }
    }

    pub fn report_frame_present(&mut self, target_timestamp: Duration, offset: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            let now = Instant::now() - offset;

            self.last_frame_present_interval =
                now.saturating_duration_since(self.last_frame_present_instant);
            self.last_frame_present_instant = now;

            frame.frame_present = now;
        }
    }

    pub fn report_frame_composed(&mut self, target_timestamp: Duration, offset: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            frame.frame_composed = Instant::now() - offset;
        }
    }

    // returns encoding interval
    pub fn report_frame_encoded(
        &mut self,
        target_timestamp: Duration,
        bytes_count: usize, is_idr: bool,
    ) -> Duration {
        self.video_packets_total += 1;
        self.video_packets_partial_sum += 1;
        self.video_bytes_total += bytes_count;
        self.video_bytes_partial_sum += bytes_count;

        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            frame.frame_encoded = Instant::now();

            frame.video_packet_bytes = bytes_count;

            frame.is_idr = is_idr;

            frame
                .frame_encoded
                .saturating_duration_since(frame.frame_composed)
        } else {
            Duration::ZERO
        }
    }

    pub fn report_packet_loss(&mut self) {
        self.packets_lost_total += 1;
        self.packets_lost_partial_sum += 1;
    }

    pub fn report_battery(&mut self, device_id: u64, gauge_value: f32, is_plugged: bool) {
        *self.battery_gauges.entry(device_id).or_default() = BatteryData {
            gauge_value,
            is_plugged,
        };
    }

    pub fn report_nominal_bitrate_stats(&mut self, stats: NominalBitrateStats) {
        self.last_nominal_bitrate_stats = stats;
    }

    // Called every frame. Some statistics are reported once every frame
    // Returns (network latency, game time latency)
    pub fn report_statistics(&mut self, client_stats: ClientStatistics) -> (Duration, Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == client_stats.target_timestamp)
        {
            frame.total_pipeline_latency = client_stats.total_pipeline_latency;

            let game_time_latency = frame
                .frame_present
                .saturating_duration_since(frame.tracking_received);

            let server_compositor_latency = frame
                .frame_composed
                .saturating_duration_since(frame.frame_present);

            let encoder_latency = frame
                .frame_encoded
                .saturating_duration_since(frame.frame_composed);

            // The network latency cannot be estiamed directly. It is what's left of the total
            // latency after subtracting all other latency intervals. In particular it contains the
            // transport latency of the tracking packet and the interval between the first video
            // packet is sent and the last video packet is received for a specific frame.
            // For safety, use saturating_sub to avoid a crash if for some reason the network
            // latency is miscalculated as negative.
            let network_latency = frame.total_pipeline_latency.saturating_sub(
                game_time_latency
                    + server_compositor_latency
                    + encoder_latency
                    + client_stats.video_decode
                    + client_stats.video_decoder_queue
                    + client_stats.rendering
                    + client_stats.vsync_queue,
            );

            let client_fps = 1.0
                / client_stats
                    .frame_interval
                    .max(Duration::from_millis(1))
                    .as_secs_f32();
            let server_fps = 1.0
                / self
                    .last_frame_present_interval
                    .max(Duration::from_millis(1))
                    .as_secs_f32();

            if self.last_full_report_instant + FULL_REPORT_INTERVAL < Instant::now() {
                self.last_full_report_instant += FULL_REPORT_INTERVAL;

                let interval_secs = FULL_REPORT_INTERVAL.as_secs_f32();

                alvr_events::send_event(EventType::StatisticsSummary(StatisticsSummary {
                    video_packets_total: self.video_packets_total,
                    video_packets_per_sec: (self.video_packets_partial_sum as f32 / interval_secs)
                        as _,
                    video_mbytes_total: (self.video_bytes_total as f32 / 1e6) as usize,
                    video_mbits_per_sec: self.video_bytes_partial_sum as f32 * 8.
                        / 1e6
                        / interval_secs,
                    total_latency_ms: client_stats.total_pipeline_latency.as_secs_f32() * 1000.,
                    network_latency_ms: network_latency.as_secs_f32() * 1000.,
                    encode_latency_ms: encoder_latency.as_secs_f32() * 1000.,
                    decode_latency_ms: client_stats.video_decode.as_secs_f32() * 1000.,
                    packets_lost_total: self.packets_lost_total,
                    packets_lost_per_sec: (self.packets_lost_partial_sum as f32 / interval_secs)
                        as _,
                    client_fps: client_fps as _,
                    server_fps: server_fps as _,
                    battery_hmd: (self
                        .battery_gauges
                        .get(&HEAD_ID)
                        .cloned()
                        .unwrap_or_default()
                        .gauge_value
                        * 100.) as u32,
                    hmd_plugged: self
                        .battery_gauges
                        .get(&HEAD_ID)
                        .cloned()
                        .unwrap_or_default()
                        .is_plugged,
                }));

                self.video_packets_partial_sum = 0;
                self.video_bytes_partial_sum = 0;
                self.packets_lost_partial_sum = 0;
            }

            // While not accurate, this prevents NaNs and zeros that would cause a crash or pollute
            // the graph
            let bitrate_bps = if network_latency != Duration::ZERO {
                frame.video_packet_bytes as f32 * 8.0 / network_latency.as_secs_f32()
            } else {
                0.0
            };
            let network_throughput_bps: f32 = if client_stats.frame_interarrival != 0.0 {
                client_stats.rx_bytes as f32 * 8.0 / client_stats.frame_interarrival 
            }     
            else{0.0}; 

            let peak_network_throughput_bps: f32 = if client_stats.frame_span != 0.0 {
                client_stats.bytes_in_frame as f32 * 8.0 / client_stats.frame_span
            }
            else{0.0}; 
            
            let application_throughput_bps = if client_stats.frame_interarrival != 0.0{
                client_stats.bytes_in_frame_app as f32 * 8.0 / client_stats.frame_interarrival
            }
            else{0.0}; 


            let mut shards_sent: usize = 0;
            // let shard_loss: 
            let mut shard_loss_server: usize = 0; 

            if self.prev_highest_frame == client_stats.highest_rx_frame_index as i32 {

                if self.prev_highest_shard < client_stats.highest_rx_shard_index as i32{
                    shards_sent =  (client_stats.highest_rx_shard_index - self.prev_highest_shard) as usize;
                    self.prev_highest_shard = client_stats.highest_rx_shard_index as i32; 
                }
                shard_loss_server = shards_sent - client_stats.rx_shard_counter as usize; 
            }
            else if self.prev_highest_frame < client_stats.highest_rx_frame_index as i32{
                let mut shards_from_prev: usize = 0;
                if let Some(shards_count_prev) = self.map_frames_spf.get(&(self.prev_highest_frame as u32)){
                    shards_from_prev = *shards_count_prev  - (self.prev_highest_shard - 1) as usize; 
                }
                
                let shards_from_inbetween_frames: usize = self.map_frames_spf.iter()
                    .filter(|&(frame, _ )| *frame > self.prev_highest_frame as u32 && *frame < client_stats.highest_rx_frame_index as u32)
                    .map(|(_, val)| *val).sum(); 

                let shards_from_actual: usize = client_stats.highest_rx_shard_index as usize + 1;

                let shards_sent = shards_from_prev + shards_from_inbetween_frames + shards_from_actual; 
                
                shard_loss_server = shards_sent - client_stats.rx_shard_counter as usize; 

                self.prev_highest_frame = client_stats.highest_rx_frame_index as i32; 
                self.prev_highest_shard = client_stats.highest_rx_shard_index as i32;


                let keys_to_drop: Vec<_> = self.map_frames_spf
                                    .iter()
                                    .filter(|&(frame,_)| *frame < self.prev_highest_frame as u32)
                                    .map(|(key, _)| *key)
                                    .collect(); 

                for key in keys_to_drop{
                    self.map_frames_spf.remove_entry(&key);
                }
            }
            // todo: use target timestamp in nanoseconds. the dashboard needs to use the first
            // timestamp as the graph time origin.
            alvr_events::send_event(EventType::GraphStatistics(GraphStatistics {
                total_pipeline_latency_s: client_stats.total_pipeline_latency.as_secs_f32(),
                game_time_s: game_time_latency.as_secs_f32(),
                server_compositor_s: server_compositor_latency.as_secs_f32(),
                encoder_s: encoder_latency.as_secs_f32(),
                network_s: network_latency.as_secs_f32(),
                decoder_s: client_stats.video_decode.as_secs_f32(),
                decoder_queue_s: client_stats.video_decoder_queue.as_secs_f32(),
                client_compositor_s: client_stats.rendering.as_secs_f32(),
                vsync_queue_s: client_stats.vsync_queue.as_secs_f32(),
                client_fps,
                server_fps,
                nominal_bitrate: self.last_nominal_bitrate_stats.clone(),
                actual_bitrate_bps: bitrate_bps,

                jitter_avg_frame: client_stats.jitter_avg_frame, 
                frame_span: client_stats.frame_span, 
                frame_interarrival: client_stats.frame_interarrival, 
                rx_bytes :          client_stats.rx_bytes, 

                network_throughput_bps: network_throughput_bps, 
                peak_network_throughput_bps: peak_network_throughput_bps, 
                application_throughput_bps: application_throughput_bps, 

                filtered_ow_delay:      client_stats.filtered_ow_delay, 
                rx_shard_counter:       client_stats.rx_shard_counter, 
                duplicated_shard_counter: client_stats.duplicated_shard_counter,
                frames_skipped:         client_stats.frames_skipped, 
                frames_dropped:         client_stats.frames_dropped, 
                frame_loss :            client_stats.frames_skipped + client_stats.frames_dropped, 

                shard_loss_server:  shard_loss_server, 
                frame_index: frame.frame_index,
                is_idr:  frame.is_idr,
                target_timestamp: client_stats.target_timestamp,

            }));

            (network_latency, game_time_latency)
        } else {
            (Duration::ZERO, Duration::ZERO)
        }
    }

    pub fn report_frame_sent(&mut self, target_timestamp: Duration, frame_sent_id: u32, spf: usize){
        if let Some(frame) = self
        .history_buffer
        .iter_mut()
        .find(|frame| frame.target_timestamp == target_timestamp)
    {
        frame.frame_index = frame_sent_id;
    }
        self.map_frames_spf.insert(frame_sent_id, spf); 
    }

    pub fn video_pipeline_latency_average(&self) -> Duration {
        self.total_pipeline_latency_average.get_average()
    }

    pub fn tracker_pose_time_offset(&self) -> Duration {
        // This is the opposite of the client's StatisticsManager::tracker_prediction_offset().
        self.steamvr_pipeline_latency
            .saturating_sub(self.total_pipeline_latency_average.get_average())
    }

    // NB: this call is non-blocking, waiting should be done externally
    pub fn duration_until_next_vsync(&mut self) -> Duration {
        let now = Instant::now();

        // update the last vsync if it's too old
        while self.last_vsync_time + self.frame_interval < now {
            self.last_vsync_time += self.frame_interval;
        }

        (self.last_vsync_time + self.frame_interval).saturating_duration_since(now)
    }
}
