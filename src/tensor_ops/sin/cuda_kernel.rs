use crate::tensor_ops::cuda_kernels::cuda_unary;

unsafe impl cudarc::driver::AsKernelParam for super::SinKernelOp {}

const PTX: &str = include_str!(concat!(env!("OUT_DIR"), "/sin.ptx"));

cuda_unary!(super::SinKernelOp, f32, PTX, "sin_fwd_f32", "sin_bwd_f32");
cuda_unary!(super::SinKernelOp, f64, PTX, "sin_fwd_f64", "sin_bwd_f64");
